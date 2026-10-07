//! Behaviour of brush dynamics on real strokes: tapers, speed, the tip's
//! turn and squash, randomness, and that they leave undo exact.

use crate::brush_engine::brush::Brush;
use crate::brush_engine::dynamics::{BrushDynamics, Randomness, SpeedDynamics, Taper, TipShape};
use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use crate::canvas::Canvas;
use crate::canvas::history::{History, UndoAction};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;

mod airbrush_and_tips;
mod depth;
mod inputs;
mod special_brushes;
mod strokes;
mod wash_tips_and_engines;

const W: usize = 256;
const H: usize = 128;

fn empty_undo() -> UndoAction {
    UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    }
}

fn brush(dynamics: BrushDynamics) -> Brush {
    let mut b = Brush::new(20.0, 100.0, Color32::BLACK, 5.0);
    b.brush_options.pressure_size = false;
    b.dynamics = dynamics;
    b
}

/// Paint `points` (position, seconds) as one stroke; `finish` lifts the pen.
fn paint(
    brush: &mut Brush,
    points: &[(Vec2, f64)],
    seed: u64,
    finish: bool,
) -> (Canvas, UndoAction) {
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(seed);
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        for &(p, t) in points {
            stroke.add_sample(brush, p, 1.0, Some(t), &mut ctx);
        }
        if finish {
            stroke.finish(brush, &mut ctx);
        }
    }
    (canvas, undo)
}

/// A horizontal line across the canvas, drawn in `seconds`.
fn line(y: f32, seconds: f64) -> Vec<(Vec2, f64)> {
    (0..=60)
        .map(|i| {
            let t = i as f32 / 60.0;
            (Vec2::new(20.0 + t * 216.0, y), t as f64 * seconds)
        })
        .collect()
}

fn alpha(canvas: &Canvas, x: usize, y: usize) -> u8 {
    canvas
        .get_layer_tile_data(1, (x / 64) as i32, (y / 64) as i32)
        .map_or(0, |t| t[(y % 64) * 64 + x % 64].a())
}

/// Rows at column `x` painted more than half.
fn thickness(canvas: &Canvas, x: usize) -> usize {
    (0..H).filter(|&y| alpha(canvas, x, y) > 127).count()
}

fn pixels(canvas: &Canvas) -> Vec<u8> {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .map(|(x, y)| alpha(canvas, x, y))
        .collect()
}

fn tapered(start: f32, end: f32) -> BrushDynamics {
    BrushDynamics {
        taper: Taper {
            start,
            end,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Width and height of what one dab paints.
fn dab_extent(tip: TipShape) -> (usize, usize) {
    let mut b = brush(BrushDynamics {
        tip,
        ..Default::default()
    });
    b.brush_options.diameter = 40.0;
    let (canvas, _) = paint(&mut b, &[(Vec2::new(128.0, 64.0), 0.0)], 1, true);
    let painted: Vec<(usize, usize)> = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|&(x, y)| alpha(&canvas, x, y) > 127)
        .collect();
    let xs = painted.iter().map(|p| p.0);
    let ys = painted.iter().map(|p| p.1);
    (
        xs.clone().max().unwrap() - xs.min().unwrap() + 1,
        ys.clone().max().unwrap() - ys.min().unwrap() + 1,
    )
}

/// A canvas whose paint layer is solid `color`.
fn painted(color: Color32) -> Canvas {
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    for ty in 0..(H / 64) as i32 {
        for tx in 0..(W / 64) as i32 {
            canvas.set_layer_tile_data(1, tx, ty, vec![color; 64 * 64]);
        }
    }
    canvas
}

/// One stroke on `canvas`.
fn paint_on(canvas: &Canvas, brush: &mut Brush, points: &[(Vec2, f64)], seed: u64) -> UndoAction {
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(seed);
    let mut ctx = StrokeContext::new(&pool, canvas, None, &mut undo, &mut tiles);
    for &(p, t) in points {
        stroke.add_sample(brush, p, 1.0, Some(t), &mut ctx);
    }
    stroke.finish(brush, &mut ctx);
    undo
}

fn pixel(canvas: &Canvas, x: usize, y: usize) -> Color32 {
    canvas
        .get_layer_tile_data(1, (x / 64) as i32, (y / 64) as i32)
        .unwrap()[(y % 64) * 64 + x % 64]
}

fn hue_random() -> BrushDynamics {
    BrushDynamics {
        random: Randomness {
            hue: 120.0,
            value: 0.2,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// One stroke with the pen at `pressure` and leaning as `tilt` throughout.
fn paint_pen(
    brush: &mut Brush,
    points: &[(Vec2, f64)],
    pressure: f32,
    tilt: Option<crate::brush_engine::dynamics::PenTilt>,
) -> Canvas {
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(1);
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        for &(p, t) in points {
            stroke.tilt = tilt;
            stroke.add_sample(brush, p, pressure, Some(t), &mut ctx);
        }
        stroke.finish(brush, &mut ctx);
    }
    canvas
}

/// A wavy stroke with changing pressure and a leaning pen, like real input.
fn wavy() -> Vec<(Vec2, f32, f64)> {
    (0..=80)
        .map(|i| {
            let t = i as f32 / 80.0;
            let pos = Vec2::new(24.0 + t * 208.0, 64.0 + (t * 9.0).sin() * 30.0);
            (
                pos,
                0.3 + 0.7 * (t * std::f32::consts::PI).sin(),
                t as f64 * 0.8,
            )
        })
        .collect()
}

fn paint_preset(brush: &mut Brush, below: Color32) -> (Canvas, UndoAction) {
    use crate::brush_engine::dynamics::PenTilt;
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let canvas = painted(below);
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(3);
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        for (p, pressure, t) in wavy() {
            stroke.tilt = Some(PenTilt {
                lean: 0.4,
                direction: 0.7,
            });
            stroke.add_sample(brush, p, pressure, Some(t), &mut ctx);
        }
        stroke.finish(brush, &mut ctx);
    }
    (canvas, undo)
}

fn pixels_rgba(canvas: &Canvas) -> Vec<Color32> {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .map(|(x, y)| pixel(canvas, x, y))
        .collect()
}

/// A press at the canvas centre held still, the airbrush called every
/// 10 ms until `seconds`; the centre's alpha after each call.
fn hold_airbrush(brush: &mut Brush, seconds: f64, seed: u64) -> (Canvas, Vec<u8>) {
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(seed);
    let mut centre = Vec::new();
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        stroke.add_sample(brush, Vec2::new(128.0, 64.0), 1.0, Some(0.0), &mut ctx);
        let mut t = 0.0;
        while t < seconds {
            t += 0.01;
            stroke.airbrush(brush, t, &mut ctx);
            centre.push(alpha(&canvas, 128, 64));
        }
        stroke.finish(brush, &mut ctx);
    }
    (canvas, centre)
}

fn airbrush(rate: f32) -> Brush {
    let mut b = Brush::new(30.0, 0.0, Color32::BLACK, 10.0);
    b.brush_options.flow = 5.0;
    b.airbrush_rate = rate;
    b
}

/// Tips that paint in different places: a dot left, centre or right of
/// the dab's centre.
fn marker_tips() -> Vec<std::sync::Arc<crate::brush_engine::tip::TipMask>> {
    (0..3)
        .map(|k| {
            let pixels = (0..32 * 32i32)
                .map(|i| {
                    let (x, y) = (i % 32, i / 32);
                    let cx = 6 + k * 10;
                    if (x - cx).abs() <= 2 && (14..18).contains(&y) {
                        255
                    } else {
                        // A faint frame (just above what trimming drops)
                        // keeps every tip the same size.
                        if x == 0 || x == 31 || y == 0 || y == 31 {
                            3
                        } else {
                            0
                        }
                    }
                })
                .collect();
            crate::brush_engine::tip::TipMask::from_mask(32, 32, pixels)
        })
        .collect()
}

fn multi_tip(order: crate::brush_engine::brush_options::TipOrder) -> Brush {
    use crate::brush_engine::brush_options::PixelBrushShape;
    let tips = marker_tips();
    let mut b = Brush::new(32.0, 100.0, Color32::BLACK, 100.0);
    b.brush_options.pressure_size = false;
    b.brush_options.pixel_shape = PixelBrushShape::Custom(tips[0].clone());
    b.brush_options.extra_tips = tips[1..].to_vec();
    b.brush_options.tip_order = order;
    b
}

/// The tip each dab of a stroke through `points` (position, pressure) used.
fn tips_used(brush: &mut Brush, points: &[(Vec2, f32)], seed: u64) -> Vec<u8> {
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let canvas = Canvas::new(W, H, Color32::WHITE, 64);
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(seed);
    let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
    for (i, &(p, pressure)) in points.iter().enumerate() {
        stroke.add_sample(brush, p, pressure, Some(i as f64 * 0.01), &mut ctx);
    }
    stroke.finish(brush, &mut ctx);
    stroke.painted.iter().map(|v| v.tip).collect()
}

fn along_x(from: f32, to: f32, steps: usize) -> Vec<(Vec2, f32)> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            (Vec2::new(from + (to - from) * t, 64.0), 1.0)
        })
        .collect()
}

fn dual_brush(dual: Option<crate::brush_engine::dual::DualTip>) -> Brush {
    let mut b = Brush::new(20.0, 60.0, Color32::BLACK, 10.0);
    b.brush_options.pressure_size = false;
    b.dual = dual;
    b
}

fn wash(wet_edge: f32) -> Brush {
    let mut b = Brush::new(50.0, 100.0, Color32::BLACK, 5.0);
    b.brush_options.pressure_size = false;
    b.brush_options.opacity = 0.6;
    b.brush_options.painting_mode = crate::brush_engine::brush_options::PaintingMode::Wash;
    b.wet_edge = wet_edge;
    b.wet_edge_width = 6.0;
    b
}

fn bristle_brush(ink: f32) -> Brush {
    let mut b = Brush::new(40.0, 80.0, Color32::BLACK, 10.0);
    b.brush_type = crate::brush_engine::brush::BrushType::Bristle;
    b.brush_options.pressure_min_size = 0.3;
    b.bristles = crate::brush_engine::bristle::Bristles {
        count: 8,
        thickness: 2.0,
        spread: 1.0,
        ink,
        variation: 0.0,
    };
    b
}

/// Separate painted runs along a column (`vertical`) or a row.
fn runs(canvas: &Canvas, at: usize, vertical: bool) -> usize {
    let len = if vertical { H } else { W };
    let painted: Vec<bool> = (0..len)
        .map(|i| {
            let (x, y) = if vertical { (at, i) } else { (i, at) };
            alpha(canvas, x, y) > 60
        })
        .collect();
    painted.windows(2).filter(|w| w[1] && !w[0]).count() + usize::from(painted[0])
}

fn stroke_with_pressure(brush: &mut Brush, points: &[(Vec2, f32)]) -> Canvas {
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(1);
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        for (i, &(p, pressure)) in points.iter().enumerate() {
            stroke.add_sample(brush, p, pressure, Some(i as f64 * 0.01), &mut ctx);
        }
        stroke.finish(brush, &mut ctx);
    }
    canvas
}

fn builtin_tip(name: &str) -> std::sync::Arc<crate::brush_engine::tip::TipMask> {
    crate::brush_engine::tip::builtin()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| t.clone())
        .unwrap()
}

/// The colour (unmultiplied sRGB) of layer pixel `(x, y)`.
fn rgb(canvas: &Canvas, x: usize, y: usize) -> [u8; 3] {
    let [r, g, b, _] = pixel(canvas, x, y).to_srgba_unmultiplied();
    [r, g, b]
}

fn ribbon_brush(tip: &str, colours: bool) -> Brush {
    use crate::brush_engine::brush_options::{PixelBrushShape, Placement};
    let mut b = Brush::new(32.0, 90.0, Color32::BLACK, 10.0);
    b.brush_options.pressure_size = false;
    b.brush_options.pixel_shape = PixelBrushShape::Custom(builtin_tip(tip));
    b.brush_options.placement = Placement::Ribbon;
    b.brush_options.tip_colors = colours;
    b
}

fn hatch_brush() -> Brush {
    let mut b = Brush::new(40.0, 100.0, Color32::BLACK, 10.0);
    b.brush_type = crate::brush_engine::brush::BrushType::Hatching;
    b.brush_options.pressure_size = false;
    b.hatching = crate::brush_engine::hatching::Hatching {
        angle: 0.0,
        separation: 6.0,
        thickness: 1.5,
        crosshatch: true,
    };
    b
}

/// Dabs at `centers` (each on its own), with wrap-around or not.
fn dabs_at(centers: &[Vec2], wrap: bool) -> Canvas {
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut brush = Brush::new(30.0, 50.0, Color32::BLACK, 10.0);
    {
        let mut ctx =
            StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles).with_wrap(wrap);
        for &c in centers {
            let mut stroke = StrokeState::with_seed(1);
            stroke.add_sample(&mut brush, c, 1.0, Some(0.0), &mut ctx);
            stroke.finish(&mut brush, &mut ctx);
        }
    }
    canvas
}

fn mapped(mappings: Vec<crate::brush_engine::dynamics::InputMapping>) -> Brush {
    let mut b = brush(BrushDynamics::default());
    b.inputs = mappings;
    b
}

fn map_pressure(
    setting: crate::brush_engine::dynamics::DabSetting,
    amount: f32,
) -> crate::brush_engine::dynamics::InputMapping {
    crate::brush_engine::dynamics::InputMapping {
        sensor: crate::brush_engine::dynamics::Sensor::Pressure,
        setting,
        amount,
        ..Default::default()
    }
}

/// Partly painted pixels down column `x` (the soft edge).
fn soft_edge(canvas: &Canvas, x: usize) -> usize {
    (0..H)
        .filter(|&y| (1..250).contains(&alpha(canvas, x, y)))
        .count()
}

fn textured() -> Brush {
    let mut b = brush(BrushDynamics::default());
    b.brush_options.diameter = 30.0;
    let mut t = crate::brush_engine::texture::BrushTexture::new(
        crate::brush_engine::texture::builtin()[1].clone(),
    );
    t.strength = 1.0;
    b.texture = Some(t);
    b
}

/// A stroke with the texture placed by `placement`, starting at `x0`.
fn placed_stroke(
    placement: crate::brush_engine::texture::GrainPlacement,
    x0: f32,
    seed: u64,
) -> Canvas {
    let mut b = textured();
    if let Some(t) = b.texture.as_mut() {
        t.placement = placement;
    }
    let points: Vec<(Vec2, f64)> = (0..=30)
        .map(|i| (Vec2::new(x0 + i as f32 * 3.0, 64.0), i as f64 * 0.01))
        .collect();
    paint(&mut b, &points, seed, true).0
}

/// How many pixels of two windows differ by more than the alpha dither
/// (which goes by canvas position).
fn unlike(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).filter(|(a, b)| a.abs_diff(**b) > 3).count()
}

/// The canvas from column `x0`, `n` wide.
fn window(c: &Canvas, x0: usize, n: usize) -> Vec<u8> {
    (0..H)
        .flat_map(|y| (x0..x0 + n).map(move |x| (x, y)))
        .map(|(x, y)| alpha(c, x, y))
        .collect()
}

/// A tip solid on its left half and faint on its right (a faint half
/// rather than none, which the tip would be cropped to).
fn left_half_tip() -> crate::brush_engine::brush_options::PixelBrushShape {
    let pixels = (0..16 * 16)
        .map(|i| if i % 16 < 8 { 255 } else { 40 })
        .collect();
    crate::brush_engine::brush_options::PixelBrushShape::Custom(
        crate::brush_engine::tip::TipMask::from_mask(16, 16, pixels),
    )
}

/// A wash brush, opacity by pressure, flow `flow` (0..100).
fn wash_brush(flow: f32) -> Brush {
    let mut b = Brush::new(20.0, 100.0, Color32::BLACK, 5.0);
    let o = &mut b.brush_options;
    o.pressure_size = false;
    o.pressure_opacity = true;
    o.flow = flow;
    o.painting_mode = crate::brush_engine::brush_options::PaintingMode::Wash;
    b
}

/// Back and forth over y = 64 `passes` times in one stroke, at `pressure`.
fn scrub(brush: &mut Brush, passes: usize, pressure: f32) -> Canvas {
    let points: Vec<(Vec2, f64)> = (0..=passes * 20)
        .map(|i| {
            let t = (i % 40) as f32 / 20.0;
            let x = if t <= 1.0 { t } else { 2.0 - t };
            (Vec2::new(40.0 + x * 160.0, 64.0), i as f64 * 0.01)
        })
        .collect();
    paint_pen(brush, &points, pressure, None)
}

/// Pixels of one dab at `pressure` that are fully painted, and ones only
/// partly.
fn dab_alphas(brush: &mut Brush, pressure: f32) -> (usize, usize) {
    let canvas = paint_pen(brush, &[(Vec2::new(128.0, 64.0), 0.0)], pressure, None);
    let all = pixels(&canvas);
    (
        all.iter().filter(|&&a| a == 255).count(),
        all.iter().filter(|&&a| a > 0 && a < 255).count(),
    )
}

/// A single dab of a hard round tip 40 px across at (128, 64), with
/// `set` applied to the brush.
fn one_dab(set: impl FnOnce(&mut Brush)) -> Canvas {
    let mut b = brush(BrushDynamics::default());
    b.brush_options.diameter = 40.0;
    set(&mut b);
    paint(&mut b, &[(Vec2::new(128.0, 64.0), 0.0)], 1, true).0
}

/// The painted (over half) pixels' width and height.
fn extent(canvas: &Canvas) -> (usize, usize) {
    let painted: Vec<(usize, usize)> = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|&(x, y)| alpha(canvas, x, y) > 127)
        .collect();
    let xs = painted.iter().map(|p| p.0);
    let ys = painted.iter().map(|p| p.1);
    (
        xs.clone().max().unwrap() - xs.min().unwrap() + 1,
        ys.clone().max().unwrap() - ys.min().unwrap() + 1,
    )
}

/// A brush of engine `t`, 30 px.
fn engine(t: crate::brush_engine::brush::BrushType) -> Brush {
    let mut b = brush(BrushDynamics::default());
    b.brush_type = t;
    b.brush_options.diameter = 30.0;
    b
}

fn covered(canvas: &Canvas) -> usize {
    pixels(canvas).iter().filter(|&&a| a > 0).count()
}
