//! Softness curve editor (brush falloff from the dab center to its edge).
//!
//! Interaction, like the curve tools in Photoshop:
//! - press on a point to grab it (picked where the press happened, so a quick
//!   drag can't miss it); points can pass their neighbours;
//! - press on empty space to add a point there and keep dragging it;
//! - right-click a point, or drag it out of the box, to remove it;
//! - the end points only move vertically.

use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
use crate::ui::style::*;
use eframe::egui::{self, Color32, Pos2, RichText, Sense, Stroke};

/// Drawn handle radius, and the margin that keeps edge handles unclipped.
const HANDLE_RADIUS: f32 = 5.0;
/// How far past the box (in points) a middle point must be dragged to delete it.
const DELETE_DISTANCE: f32 = 36.0;
/// Closest two points may get horizontally.
const MIN_GAP: f32 = 0.002;

/// A named quick shape offered under the editor.
pub(crate) type CurvePreset = (&'static str, &'static [(f32, f32)]);

/// Quick falloff shapes (centre to edge).
const PRESETS: [CurvePreset; 4] = [
    ("Linear", &[(0.0, 1.0), (1.0, 0.0)]),
    ("Soft", &[(0.0, 1.0), (0.25, 0.5), (0.6, 0.12), (1.0, 0.0)]),
    (
        "Round",
        &[(0.0, 1.0), (0.5, 0.85), (0.85, 0.35), (1.0, 0.0)],
    ),
    ("Hard", &[(0.0, 1.0), (0.75, 0.97), (0.92, 0.4), (1.0, 0.0)]),
];

/// Input responses for brush inputs: a direction's "Peak" makes one way of
/// travel the strongest and the opposite the weakest.
pub(crate) const INPUT_PRESETS: [CurvePreset; 4] = [
    ("Linear", &[(0.0, 0.0), (1.0, 1.0)]),
    ("Reverse", &[(0.0, 1.0), (1.0, 0.0)]),
    ("Soft", &[(0.0, 0.0), (0.55, 0.2), (1.0, 1.0)]),
    ("Peak", &[(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]),
];

#[derive(Clone, Copy, Default)]
struct DragState {
    /// Index of the grabbed point (kept up to date as points pass each other).
    point: Option<usize>,
}

/// Pressure responses (pen pressure in, effect out).
pub(crate) const PRESSURE_PRESETS: [CurvePreset; 4] = [
    ("Linear", &[(0.0, 0.0), (1.0, 1.0)]),
    ("Soft", &[(0.0, 0.0), (0.55, 0.2), (1.0, 1.0)]),
    ("Firm", &[(0.0, 0.0), (0.3, 0.65), (1.0, 1.0)]),
    (
        "S-curve",
        &[(0.0, 0.0), (0.3, 0.12), (0.7, 0.88), (1.0, 1.0)],
    ),
];

/// The falloff editor (centre to edge).
pub(crate) fn curve_editor(ui: &mut egui::Ui, curve: &mut SoftnessCurve) -> bool {
    curve_editor_with(ui, curve, &PRESETS)
}

/// A curve editor with `presets` as its quick shapes.
pub(crate) fn curve_editor_with(
    ui: &mut egui::Ui,
    curve: &mut SoftnessCurve,
    presets: &[CurvePreset],
) -> bool {
    let touch = metrics(ui.ctx()).touch;
    let hit_radius = if touch { 24.0 } else { 12.0 };
    let mut changed = false;

    let width = ui.available_width();
    let height = (width * 0.62).clamp(110.0, 220.0);
    let (response, painter) =
        ui.allocate_painter(egui::vec2(width, height), Sense::click_and_drag());
    let outer = response.rect;
    let rect = outer.shrink(HANDLE_RADIUS + 2.0);

    let to_screen = |p: &CurvePoint| {
        Pos2::new(
            rect.min.x + p.x * rect.width(),
            rect.max.y - p.y * rect.height(),
        )
    };
    let from_screen = |pos: Pos2| CurvePoint {
        x: ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
        y: ((rect.max.y - pos.y) / rect.height()).clamp(0.0, 1.0),
    };
    let nearest = |curve: &SoftnessCurve, pos: Pos2| {
        curve
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, to_screen(p).distance(pos)))
            .filter(|(_, d)| *d <= hit_radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };

    let state_id = response.id.with("curve_drag");
    let mut state: DragState = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
    let (pressed, down, press_origin, pointer) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.press_origin(),
            i.pointer.interact_pos(),
        )
    });

    // Press: grab the nearest point, or add one where the press happened.
    if pressed
        && state.point.is_none()
        && let Some(origin) = press_origin
        && outer.contains(origin)
    {
        state.point = match nearest(curve, origin) {
            Some(i) => Some(i),
            None => {
                let p = from_screen(origin);
                let last = curve.points.len().saturating_sub(1);
                let at = curve
                    .points
                    .iter()
                    .position(|q| q.x > p.x)
                    .unwrap_or(last)
                    .clamp(1, last.max(1));
                let x = p.x.clamp(MIN_GAP, 1.0 - MIN_GAP);
                curve.points.insert(at, CurvePoint::new(x, p.y));
                changed = true;
                Some(at)
            }
        };
    }

    // Right-click removes a middle point.
    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(i) = nearest(curve, pos)
        && i > 0
        && i + 1 < curve.points.len()
    {
        curve.points.remove(i);
        changed = true;
    }

    // Drag: follow the pointer; middle points may pass their neighbours.
    let mut delete_pending = false;
    if let Some(mut idx) = state.point {
        if !down || idx >= curve.points.len() {
            // Released: a middle point dragged out of the box is deleted.
            if idx > 0 && idx + 1 < curve.points.len() && pointer.is_some_and(|p| outside(rect, p))
            {
                curve.points.remove(idx);
                changed = true;
            }
            state.point = None;
        } else if let Some(pos) = pointer {
            let target = from_screen(pos);
            let last = curve.points.len() - 1;
            if idx == 0 || idx == last {
                curve.points[idx].y = target.y;
            } else {
                curve.points[idx] =
                    CurvePoint::new(target.x.clamp(MIN_GAP, 1.0 - MIN_GAP), target.y);
                // Keep points sorted by x, tracking the grabbed one.
                while idx > 1 && curve.points[idx].x < curve.points[idx - 1].x {
                    curve.points.swap(idx, idx - 1);
                    idx -= 1;
                }
                while idx + 1 < last && curve.points[idx].x > curve.points[idx + 1].x {
                    curve.points.swap(idx, idx + 1);
                    idx += 1;
                }
                delete_pending = outside(rect, pos);
            }
            state.point = Some(idx);
            changed = true;
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|d| d.insert_temp(state_id, state));

    // --- Drawing ---
    painter.rect_filled(outer, RADIUS_WIDGET, BG_INSET);
    for q in [0.25, 0.5, 0.75] {
        let x = egui::lerp(rect.x_range(), q);
        let y = egui::lerp(rect.y_range(), q);
        painter.vline(
            x,
            rect.y_range(),
            Stroke::new(1.0_f32, BORDER_LIGHT.gamma_multiply(0.5)),
        );
        painter.hline(
            rect.x_range(),
            y,
            Stroke::new(1.0_f32, BORDER_LIGHT.gamma_multiply(0.5)),
        );
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, BORDER_LIGHT));

    if curve.points.len() >= 2 {
        let points: Vec<Pos2> = (0..=120)
            .map(|i| {
                let t = i as f32 / 120.0;
                to_screen(&CurvePoint::new(t, curve.eval(t)))
            })
            .collect();
        // Fill under the curve: a hint of the dab's opacity profile.
        let mut fill = points.clone();
        fill.push(rect.right_bottom());
        fill.push(rect.left_bottom());
        painter.add(egui::Shape::Path(egui::epaint::PathShape {
            points: fill,
            closed: true,
            fill: accent().gamma_multiply(0.12),
            stroke: egui::epaint::PathStroke::NONE,
        }));
        painter.add(egui::Shape::line(points, Stroke::new(2.0_f32, accent())));
    }

    let hovered_point = response.hover_pos().and_then(|p| nearest(curve, p));
    for (i, p) in curve.points.iter().enumerate() {
        let center = to_screen(p);
        let grabbed = state.point == Some(i);
        let radius = if grabbed || hovered_point == Some(i) {
            HANDLE_RADIUS + 1.5
        } else {
            HANDLE_RADIUS
        };
        let fill = if grabbed && delete_pending {
            Color32::from_rgb(214, 76, 76)
        } else if grabbed {
            accent()
        } else {
            TEXT_STRONG
        };
        painter.rect_filled(
            egui::Rect::from_center_size(center, egui::vec2(radius, radius) * 2.0),
            0.0,
            fill,
        );
        painter.rect_stroke(
            egui::Rect::from_center_size(center, egui::vec2(radius, radius) * 2.0),
            0.0,
            Stroke::new(1.0_f32, Color32::BLACK),
        );
    }

    // Readout for the grabbed or hovered point.
    let readout = state
        .point
        .or(hovered_point)
        .and_then(|i| curve.points.get(i))
        .map(|p| {
            if delete_pending {
                "Release to remove".to_string()
            } else {
                format!(
                    "{:.0}% from center: {:.0}% opacity",
                    p.x * 100.0,
                    p.y * 100.0
                )
            }
        });
    if let Some(text) = readout {
        painter.text(
            rect.right_top() + egui::vec2(-4.0, 4.0),
            egui::Align2::RIGHT_TOP,
            text,
            egui::TextStyle::Small.resolve(ui.style()),
            TEXT,
        );
    }
    if hovered_point.is_some() || state.point.is_some() {
        ui.ctx().set_cursor_icon(if state.point.is_some() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }

    // Quick shapes.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for &(name, shape) in presets {
            if ui.small_button(name).clicked() {
                curve.points = shape.iter().map(|&(x, y)| CurvePoint::new(x, y)).collect();
                changed = true;
            }
        }
    });
    ui.label(
        RichText::new("Click to add · drag to move · right-click or drag out to remove")
            .small()
            .color(TEXT_DIM),
    );

    changed
}

/// Whether `pos` is far enough outside `rect` to mean "remove".
fn outside(rect: egui::Rect, pos: Pos2) -> bool {
    !rect.expand(DELETE_DISTANCE).contains(pos)
}
