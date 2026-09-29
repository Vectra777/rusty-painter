//! The grid over the canvas (View → Grid), square or isometric, and the
//! pixel grid shown when zoomed in far.
//!
//! Lines are drawn as overlays, so they stay one screen pixel wide at any
//! zoom or rotation. Zoomed out, the subdivisions fade away and then the
//! main lines thin out (every 2nd, 4th… line), so the grid never floods the
//! screen.

use crate::app::PainterApp;
use crate::app::view::render::ScreenMap;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

/// Closest two drawn lines may come on screen, in points.
pub(crate) const MIN_GAP: f32 = 8.0;
/// The pixel grid shows from this zoom (800%) up.
pub(crate) const PIXEL_GRID_ZOOM: f32 = 8.0;

/// Grid settings (kept between sessions with the other view settings).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GridSettings {
    /// The grid is shown.
    pub show: bool,
    /// Between main lines, in canvas pixels.
    pub spacing: f32,
    /// Parts each main square is divided into (1: none).
    pub subdivisions: u32,
    /// sRGB colour of the lines.
    pub color: [u8; 3],
    pub opacity: f32,
    /// Triangles (lines upright and at ±30°) instead of squares.
    pub isometric: bool,
    /// Show every pixel's edges when zoomed in to 800% or more.
    pub pixel_grid: bool,
}

impl Default for GridSettings {
    fn default() -> Self {
        Self {
            show: false,
            spacing: 100.0,
            subdivisions: 4,
            color: [120, 120, 120],
            opacity: 0.6,
            isometric: false,
            pixel_grid: true,
        }
    }
}

/// The lines drawn at a zoom: main lines every `major` canvas pixels and,
/// when they're far enough apart on screen, subdivisions every `minor`
/// with their opacity (fading in as they spread out).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GridSteps {
    pub major: f32,
    pub minor: Option<(f32, f32)>,
}

impl GridSteps {
    /// The finest lines drawn (what snapping snaps to).
    pub fn finest(self) -> f32 {
        self.minor.map_or(self.major, |(step, _)| step)
    }
}

/// Which lines of a grid of `spacing` with `subdivisions` to draw at
/// `zoom` (screen points per canvas pixel): none closer than [`MIN_GAP`]
/// on screen. Zoomed out, main lines are skipped two at a time (every 2nd,
/// then 4th…), so the ones left still fall on the grid.
pub(crate) fn grid_steps(spacing: f32, subdivisions: u32, zoom: f32) -> Option<GridSteps> {
    if !(spacing > 0.0 && zoom > 0.0 && spacing.is_finite() && zoom.is_finite()) {
        return None;
    }
    let mut major = spacing;
    while major * zoom < MIN_GAP {
        major *= 2.0;
    }
    let minor = (subdivisions > 1 && major == spacing)
        .then(|| {
            let step = spacing / subdivisions as f32;
            let fade = ((step * zoom - MIN_GAP) / MIN_GAP).clamp(0.0, 1.0);
            (step, fade)
        })
        .filter(|&(_, fade)| fade > 0.0);
    Some(GridSteps { major, minor })
}

/// How strongly the pixel grid shows at `zoom`, if at all: from nothing
/// at 800% to fully at 1600%.
pub(crate) fn pixel_grid_fade(zoom: f32) -> Option<f32> {
    (zoom >= PIXEL_GRID_ZOOM)
        .then(|| (0.4 + 0.6 * (zoom - PIXEL_GRID_ZOOM) / PIXEL_GRID_ZOOM).min(1.0))
}

/// Unit normals of the grid's line families: lines are the points `p`
/// with `p · n` a multiple of the step.
fn families(isometric: bool) -> &'static [Vec2] {
    const SQUARE: [Vec2; 2] = [Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)];
    // Upright lines and lines at ±30° from horizontal: a triangle lattice
    // (n1 = n2 − n0, so two families' crossings lie on the third).
    const ISO: [Vec2; 3] = [
        Vec2::new(1.0, 0.0),
        Vec2::new(-0.5, 0.866_025_4),
        Vec2::new(0.5, 0.866_025_4),
    ];
    if isometric { &ISO } else { &SQUARE }
}

/// `p` moved onto the grid of `step` if it's within `threshold` (canvas
/// pixels) of it: onto a crossing when near one, else onto the nearest
/// line (a square grid snaps each axis on its own).
pub(crate) fn snap_to_grid(p: Vec2, step: f32, isometric: bool, threshold: f32) -> Vec2 {
    if step <= 0.0 {
        return p;
    }
    if !isometric {
        let axis = |v: f32| {
            let on = (v / step).round() * step;
            if (on - v).abs() <= threshold { on } else { v }
        };
        return Vec2::new(axis(p.x), axis(p.y));
    }
    let (n0, n2) = (families(true)[0], families(true)[2]);
    // The crossing with `v · n0 = a·step` and `v · n2 = b·step`.
    let vertex = |a: f32, b: f32| {
        let x = a * step;
        Vec2::new(x, (b * step - n2.x * x) / n2.y)
    };
    let (a0, b0) = ((p.dot(n0) / step).round(), (p.dot(n2) / step).round());
    let nearest = (-1..=1)
        .flat_map(|da| (-1..=1).map(move |db| vertex(a0 + da as f32, b0 + db as f32)))
        .min_by(|u, v| (*u - p).length().total_cmp(&(*v - p).length()));
    if let Some(v) = nearest
        && (v - p).length() <= threshold
    {
        return v;
    }
    families(true)
        .iter()
        .map(|&n| {
            let d = p.dot(n);
            (n, (d / step).round() * step - d)
        })
        .filter(|(_, off)| off.abs() <= threshold)
        .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map_or(p, |(n, off)| p + n * off)
}

/// The part of the line `p · n = d` inside `area`, if any.
fn line_in(area: Rect, n: Vec2, d: f32) -> Option<(Vec2, Vec2)> {
    let dir = Vec2::new(-n.y, n.x);
    let origin = n * d;
    let (mut lo, mut hi) = (f32::NEG_INFINITY, f32::INFINITY);
    for (o, v, min, max) in [
        (origin.x, dir.x, area.min.x, area.max.x),
        (origin.y, dir.y, area.min.y, area.max.y),
    ] {
        if v.abs() < 1e-6 {
            if o < min || o > max {
                return None;
            }
            continue;
        }
        let (a, b) = ((min - o) / v, (max - o) / v);
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
    }
    (lo < hi).then(|| (origin + dir * lo, origin + dir * hi))
}

/// Canvas area (clipped to the canvas) that shows in `screen`.
fn visible_canvas(map: &ScreenMap, screen: Rect, width: f32, height: f32) -> Option<Rect> {
    let corners = [
        screen.left_top(),
        screen.right_top(),
        screen.right_bottom(),
        screen.left_bottom(),
    ]
    .map(|c| map.to_canvas(c));
    let seen = Rect::from_points(&corners.map(|c| egui::pos2(c.x, c.y)));
    let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(width, height));
    let area = seen.intersect(canvas);
    area.is_positive().then_some(area)
}

/// Every line of the families `normals` spaced `step` apart within `area`.
fn draw_lines(
    painter: &egui::Painter,
    map: &ScreenMap,
    area: Rect,
    normals: &[Vec2],
    step: f32,
    stroke: Stroke,
    skip_every: Option<f32>,
) {
    let ppp = painter.ctx().pixels_per_point();
    // A line along the screen's axes sits on a pixel centre: crisp.
    let crisp = |v: f32| ((v * ppp).floor() + 0.5) / ppp;
    let corners = [
        area.left_top(),
        area.right_top(),
        area.right_bottom(),
        area.left_bottom(),
    ]
    .map(|c| c.to_vec2());
    for &n in normals {
        let (lo, hi) = corners.iter().fold((f32::MAX, f32::MIN), |(lo, hi), c| {
            (lo.min(c.dot(n)), hi.max(c.dot(n)))
        });
        let (first, last) = ((lo / step).ceil() as i64, (hi / step).floor() as i64);
        for k in first..=last {
            let d = k as f32 * step;
            // Main lines are drawn on their own.
            if let Some(major) = skip_every
                && ((d / major).round() * major - d).abs() < step * 0.01
            {
                continue;
            }
            let Some((a, b)) = line_in(area, n, d) else {
                continue;
            };
            let (mut a, mut b) = (map.to_screen(a), map.to_screen(b));
            if (a.x - b.x).abs() < 0.01 {
                a.x = crisp(a.x);
                b.x = a.x;
            } else if (a.y - b.y).abs() < 0.01 {
                a.y = crisp(a.y);
                b.y = a.y;
            }
            painter.line_segment([a, b], stroke);
        }
    }
}

/// Draw the pixel grid (zoomed in far) and the grid, under the other
/// overlays. `screen` is the canvas panel.
pub(crate) fn draw_grid(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap, screen: Rect) {
    let grid = &app.workspace.view_aids.grid;
    let zoom = map.zoom();
    let pixel_fade = pixel_grid_fade(zoom).filter(|_| grid.pixel_grid);
    if !grid.show && pixel_fade.is_none() {
        return;
    }
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let Some(area) = visible_canvas(map, screen, w, h) else {
        return;
    };
    if let Some(fade) = pixel_fade {
        let stroke = Stroke::new(1.0_f32, Color32::from_gray(128).gamma_multiply(0.35 * fade));
        draw_lines(painter, map, area, families(false), 1.0, stroke, None);
    }
    if !grid.show {
        return;
    }
    let Some(steps) = grid_steps(grid.spacing, grid.subdivisions, zoom) else {
        return;
    };
    let [r, g, b] = grid.color;
    let color = Color32::from_rgb(r, g, b).gamma_multiply(grid.opacity.clamp(0.0, 1.0));
    let normals = families(grid.isometric);
    if let Some((step, fade)) = steps.minor {
        let stroke = Stroke::new(1.0_f32, color.gamma_multiply(0.45 * fade));
        draw_lines(painter, map, area, normals, step, stroke, Some(steps.major));
    }
    draw_lines(
        painter,
        map,
        area,
        normals,
        steps.major,
        Stroke::new(1.0_f32, color),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_thin_out_when_zoomed_out() {
        // 100%: main lines 100 px apart, quarters 25 px apart, fully shown.
        let s = grid_steps(100.0, 4, 1.0).unwrap();
        assert_eq!(s.major, 100.0);
        assert_eq!(s.minor, Some((25.0, 1.0)));
        // 50%: quarters 12.5 points apart, half faded in.
        let s = grid_steps(100.0, 4, 0.5).unwrap();
        assert_eq!(s.minor.map(|m| m.0), Some(25.0));
        assert!((s.minor.unwrap().1 - 0.5625).abs() < 1e-4);
        // 25%: quarters too close, only main lines.
        let s = grid_steps(100.0, 4, 0.25).unwrap();
        assert_eq!((s.major, s.minor), (100.0, None));
        // 5%: every 2nd main line (10 points apart), then every 4th at 3%.
        assert_eq!(grid_steps(100.0, 4, 0.05).unwrap().major, 200.0);
        assert_eq!(grid_steps(100.0, 4, 0.03).unwrap().major, 400.0);
        // Never closer than the minimum gap, whatever the zoom.
        for zoom in [0.001, 0.02, 0.3, 1.0, 7.0, 32.0] {
            let s = grid_steps(10.0, 5, zoom).unwrap();
            assert!(s.finest() * zoom >= MIN_GAP, "{zoom}: {s:?}");
        }
        assert!(grid_steps(0.0, 4, 1.0).is_none());
    }

    #[test]
    fn the_pixel_grid_shows_from_800_percent() {
        assert!(pixel_grid_fade(7.9).is_none());
        assert!(pixel_grid_fade(8.0).unwrap() > 0.0);
        assert_eq!(pixel_grid_fade(32.0), Some(1.0));
    }

    #[test]
    fn points_snap_to_nearby_grid_lines_only() {
        let snap = |x, y| snap_to_grid(Vec2::new(x, y), 50.0, false, 4.0);
        assert_eq!(snap(103.0, 77.0), Vec2::new(100.0, 77.0));
        assert_eq!(snap(96.5, 148.0), Vec2::new(100.0, 150.0));
        // Beyond the threshold: left alone.
        assert_eq!(snap(105.0, 75.0), Vec2::new(105.0, 75.0));
    }

    #[test]
    fn isometric_points_snap_to_crossings_then_lines() {
        let step = 40.0;
        // A crossing: on all three families' lines.
        let v = snap_to_grid(Vec2::new(41.0, 22.0), step, true, 6.0);
        for n in families(true) {
            let d = v.dot(*n) / step;
            assert!((d - d.round()).abs() < 1e-3, "{v:?} off a line");
        }
        // Between crossings, near an upright line: onto it.
        let p = Vec2::new(82.0, 23.0);
        let on = snap_to_grid(p, step, true, 3.0);
        assert!((on.x - 80.0).abs() < 1e-3 && on.y == 23.0, "{on:?}");
        // Far from everything: left alone.
        let p = Vec2::new(60.0, 0.0);
        assert_eq!(snap_to_grid(p, step, true, 3.0), p);
    }

    #[test]
    fn lines_are_clipped_to_the_area() {
        let area = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        let (a, b) = line_in(area, Vec2::new(1.0, 0.0), 30.0).unwrap();
        assert_eq!((a.x, b.x), (30.0, 30.0));
        assert_eq!((a.y.min(b.y), a.y.max(b.y)), (0.0, 50.0));
        assert!(line_in(area, Vec2::new(0.0, 1.0), 60.0).is_none());
    }
}
