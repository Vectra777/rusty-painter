//! The brush cursor: the outline of the brush's tip under the pointer, at
//! the size and turn a dab would have there, so it shows what a stroke (or
//! the eraser) will cover before the pen touches.

use crate::app::view::render::ScreenMap;
use crate::brush_engine::brush::{Brush, BrushType};
use crate::brush_engine::brush_options::PixelBrushShape;
use crate::brush_engine::tip::TipMask;
use eframe::egui::{self, Color32, Stroke, Vec2};
use std::sync::Arc;

/// Line segments in the tip's frame, in brush radii (the tip's longest side
/// spans -1..=1).
type Outline = Arc<Vec<[[f32; 2]; 2]>>;

/// The longest side of the grid an image tip is traced on: enough for its
/// shape, few enough lines to draw every frame.
const TRACE_SIDE: usize = 64;
/// Coverage (of the tip's strongest) that counts as inside the outline.
const TRACE_LEVEL: f32 = 0.2;
const CIRCLE_POINTS: usize = 64;
/// Below this many screen points across, the outline is too small to see:
/// a crosshair marks the spot as well.
const SMALL: f32 = 6.0;

fn circle() -> Vec<[[f32; 2]; 2]> {
    let at = |i: usize| {
        let a = i as f32 / CIRCLE_POINTS as f32 * std::f32::consts::TAU;
        [a.cos(), a.sin()]
    };
    (0..CIRCLE_POINTS).map(|i| [at(i), at(i + 1)]).collect()
}

fn square() -> Vec<[[f32; 2]; 2]> {
    let c = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
    (0..4).map(|i| [c[i], c[(i + 1) % 4]]).collect()
}

/// The edge of an image tip, by marching squares over its mask averaged
/// down to at most [`TRACE_SIDE`] cells a side.
pub(crate) fn trace(tip: &TipMask) -> Vec<[[f32; 2]; 2]> {
    let (w, h) = (tip.width.max(1), tip.height.max(1));
    let longest = w.max(h);
    let block = longest.div_ceil(TRACE_SIDE).max(1);
    let (gw, gh) = (w.div_ceil(block), h.div_ceil(block));
    // Cell averages, with a clear border so shapes touching the edge close.
    let (pw, ph) = (gw + 2, gh + 2);
    let mut grid = vec![0.0f32; pw * ph];
    let mut peak = 0.0f32;
    for gy in 0..gh {
        for gx in 0..gw {
            let (x0, y0) = (gx * block, gy * block);
            let (x1, y1) = ((x0 + block).min(w), (y0 + block).min(h));
            let mut sum = 0u32;
            for y in y0..y1 {
                for &v in &tip.pixels[y * w + x0..y * w + x1] {
                    sum += v as u32;
                }
            }
            let v = sum as f32 / ((x1 - x0) * (y1 - y0)) as f32;
            peak = peak.max(v);
            grid[(gy + 1) * pw + gx + 1] = v;
        }
    }
    if peak <= 0.0 {
        return Vec::new();
    }
    let level = peak * TRACE_LEVEL;
    // Grid point (gx, gy) is the middle of its cell, in brush radii.
    let unit = 2.0 / longest as f32;
    let pos = |gx: f32, gy: f32| {
        [
            ((gx - 0.5) * block as f32 - w as f32 * 0.5) * unit,
            ((gy - 0.5) * block as f32 - h as f32 * 0.5) * unit,
        ]
    };
    let mut out = Vec::new();
    for y in 0..ph - 1 {
        for x in 0..pw - 1 {
            let v = [
                grid[y * pw + x],
                grid[y * pw + x + 1],
                grid[(y + 1) * pw + x + 1],
                grid[(y + 1) * pw + x],
            ];
            let inside = v.map(|v| v >= level);
            if inside.iter().all(|&i| i) || inside.iter().all(|&i| !i) {
                continue;
            }
            let corner = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
            // Where the level crosses each side (corner k to k + 1).
            let mut cuts = Vec::with_capacity(4);
            for k in 0..4 {
                let n = (k + 1) % 4;
                if inside[k] != inside[n] {
                    let t = (level - v[k]) / (v[n] - v[k]);
                    let (ax, ay) = corner[k];
                    let (bx, by) = corner[n];
                    cuts.push(pos(
                        x as f32 + ax + (bx - ax) * t,
                        y as f32 + ay + (by - ay) * t,
                    ));
                }
            }
            for pair in cuts.as_chunks::<2>().0 {
                out.push([pair[0], pair[1]]);
            }
        }
    }
    out
}

/// The tip's outline, traced once per tip and kept.
fn outline(ctx: &egui::Context, brush: &Brush) -> Outline {
    let shape = match brush.brush_type {
        BrushType::Soft | BrushType::Pixel => &brush.brush_options.pixel_shape,
        // Hairs and lines spread over the brush's size, whatever its tip.
        BrushType::Bristle | BrushType::Sketch | BrushType::Hatching => &PixelBrushShape::Circle,
    };
    // A tip is known by its address and size (an address can be reused).
    let stamp = match shape {
        PixelBrushShape::Circle => (0, 0, 0),
        PixelBrushShape::Square => (1, 0, 0),
        PixelBrushShape::Custom(tip) => (Arc::as_ptr(tip) as usize, tip.width, tip.height),
    };
    let id = egui::Id::new("brush_cursor_outline");
    if let Some((seen, lines)) = ctx.data(|d| d.get_temp::<((usize, usize, usize), Outline)>(id))
        && seen == stamp
    {
        return lines;
    }
    let mut lines = match shape {
        PixelBrushShape::Circle => circle(),
        PixelBrushShape::Square => square(),
        PixelBrushShape::Custom(tip) => trace(tip),
    };
    if lines.is_empty() {
        lines = circle();
    }
    let lines: Outline = Arc::new(lines);
    ctx.data_mut(|d| d.insert_temp(id, (stamp, lines.clone())));
    lines
}

/// Draw the brush's outline at screen point `pos`. Returns whether it's
/// big enough to stand in for the pointer.
pub(crate) fn draw(
    ctx: &egui::Context,
    painter: &egui::Painter,
    map: ScreenMap,
    brush: &Brush,
    pos: egui::Pos2,
) -> bool {
    let lines = outline(ctx, brush);
    let r = brush.brush_options.diameter * 0.5;
    // Tip frame to canvas: squash, then turn (the inverse of the matrix
    // dabs use, [`crate::brush_engine::dynamics::tip_orientation`]).
    let tip = &brush.dynamics.tip;
    let (s, c) = tip.angle.to_radians().sin_cos();
    let ratio = tip.ratio.clamp(0.02, 1.0);
    let centre = map.to_canvas(pos);
    let place = |p: [f32; 2]| {
        let (u, v) = (p[0] * r, p[1] * r * ratio);
        map.to_screen(centre + Vec2::new(c * u + s * v, -s * u + c * v))
    };
    let segments: Vec<[egui::Pos2; 2]> = lines.iter().map(|l| [place(l[0]), place(l[1])]).collect();
    // Dark under light, so it shows on any picture.
    for (width, colour) in [
        (2.5_f32, Color32::from_black_alpha(170)),
        (1.0, Color32::WHITE),
    ] {
        let stroke = Stroke::new(width, colour);
        for seg in &segments {
            painter.line_segment(*seg, stroke);
        }
    }
    let across = brush.brush_options.diameter * ratio * map.zoom();
    if across < SMALL {
        for (width, colour) in [
            (2.5_f32, Color32::from_black_alpha(170)),
            (1.0, Color32::WHITE),
        ] {
            let stroke = Stroke::new(width, colour);
            for d in [Vec2::X, Vec2::Y] {
                for sign in [-1.0, 1.0] {
                    painter.line_segment([pos + d * sign * 5.0, pos + d * sign * 10.0], stroke);
                }
            }
        }
    }
    across >= SMALL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(lines: &[[[f32; 2]; 2]]) -> ([f32; 2], [f32; 2]) {
        let mut lo = [f32::MAX; 2];
        let mut hi = [f32::MIN; 2];
        for p in lines.iter().flatten() {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (lo, hi)
    }

    #[test]
    fn an_image_tip_is_outlined_at_its_own_shape_and_size() {
        // A wide bar: all of a 200×50 tip.
        let tip = TipMask::from_mask(200, 50, vec![255; 200 * 50]);
        let lines = trace(&tip);
        assert!(!lines.is_empty());
        let (lo, hi) = bounds(&lines);
        // The long side spans the brush's diameter, the short a quarter.
        assert!(
            (lo[0] + 1.0).abs() < 0.06 && (hi[0] - 1.0).abs() < 0.06,
            "{lo:?} {hi:?}"
        );
        assert!(
            (lo[1] + 0.25).abs() < 0.06 && (hi[1] - 0.25).abs() < 0.06,
            "{lo:?} {hi:?}"
        );
    }

    #[test]
    fn a_hole_in_the_tip_is_outlined_too() {
        let n = 96;
        let ring: Vec<u8> = (0..n * n)
            .map(|i| {
                let (x, y) = ((i % n) as f32 - 47.5, (i / n) as f32 - 47.5);
                let d = (x * x + y * y).sqrt();
                if (24.0..46.0).contains(&d) { 255 } else { 0 }
            })
            .collect();
        let lines = trace(&TipMask::from_mask(n, n, ring));
        // Some of the outline is well inside: the hole's edge.
        let inner = lines
            .iter()
            .filter(|l| (l[0][0].powi(2) + l[0][1].powi(2)).sqrt() < 0.65)
            .count();
        assert!(inner > 8, "{inner} of {}", lines.len());
    }

    #[test]
    fn an_empty_tip_has_no_outline() {
        assert!(trace(&TipMask::from_mask(8, 8, vec![0; 64])).is_empty());
    }
}
