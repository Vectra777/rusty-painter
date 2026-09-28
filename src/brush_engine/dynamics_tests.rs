//! Behaviour of brush dynamics on real strokes: tapers, speed, the tip's
//! turn and squash, randomness, and that they leave undo exact.

use crate::brush_engine::brush::Brush;
use crate::brush_engine::dynamics::{BrushDynamics, Randomness, SpeedDynamics, Taper, TipShape};
use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use crate::canvas::Canvas;
use crate::canvas::history::{History, UndoAction};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;

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

#[test]
fn tapers_thin_both_ends() {
    let (canvas, _) = paint(&mut brush(tapered(60.0, 60.0)), &line(64.0, 0.5), 1, true);
    let (mid, start, end) = (
        thickness(&canvas, 128),
        thickness(&canvas, 26),
        thickness(&canvas, 230),
    );
    assert!((18..=21).contains(&mid), "full width in the middle: {mid}");
    assert!(start < mid / 2, "thin start: {start} vs {mid}");
    assert!(end < mid / 2, "thin end: {end} vs {mid}");
    let (plain, _) = paint(
        &mut brush(BrushDynamics::default()),
        &line(64.0, 0.5),
        1,
        true,
    );
    assert_eq!(
        thickness(&plain, 230),
        thickness(&plain, 128),
        "no taper, no thinning"
    );
}

#[test]
fn the_end_reaches_the_pen_until_it_lifts() {
    let mut b = brush(tapered(0.0, 60.0));
    let (drawing, _) = paint(&mut b, &line(64.0, 0.5), 1, false);
    let (done, _) = paint(&mut b, &line(64.0, 0.5), 1, true);
    let full = thickness(&drawing, 128);
    assert_eq!(
        thickness(&drawing, 230),
        full,
        "no lag: full width at the pen"
    );
    assert!(thickness(&done, 230) < full / 2, "thinned once lifted");
}

#[test]
fn the_redrawn_tail_leaves_nothing_behind() {
    // Away from the end, an end taper changes nothing: the tail drawn and
    // cleared at every sample must leave exactly the plain stroke.
    let (with_tail, _) = paint(&mut brush(tapered(0.0, 60.0)), &line(64.0, 0.5), 3, true);
    let (plain, _) = paint(&mut brush(tapered(0.0, 0.0)), &line(64.0, 0.5), 3, true);
    for y in 0..H {
        for x in 0..150 {
            assert_eq!(
                alpha(&with_tail, x, y),
                alpha(&plain, x, y),
                "pixel ({x}, {y})"
            );
        }
    }
}

#[test]
fn fast_strokes_thin_when_speed_does() {
    let dynamics = BrushDynamics {
        speed: SpeedDynamics {
            size: -0.8,
            opacity: 0.0,
        },
        ..Default::default()
    };
    let (slow, _) = paint(&mut brush(dynamics), &line(64.0, 5.0), 1, true);
    let (fast, _) = paint(&mut brush(dynamics), &line(64.0, 0.05), 1, true);
    let (s, f) = (thickness(&slow, 128), thickness(&fast, 128));
    assert!(f + 6 <= s, "fast {f} thinner than slow {s}");
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

#[test]
fn a_squashed_tip_is_flat_and_turns_with_its_angle() {
    let flat = TipShape {
        ratio: 0.25,
        ..Default::default()
    };
    let (w, h) = dab_extent(flat);
    assert!((38..=41).contains(&w) && (9..=11).contains(&h), "{w}×{h}");
    let (w, h) = dab_extent(TipShape {
        angle: 90.0,
        ..flat
    });
    assert!(
        (9..=11).contains(&w) && (38..=41).contains(&h),
        "turned: {w}×{h}"
    );
}

#[test]
fn a_nib_that_follows_the_stroke_draws_thin_along_it() {
    // A flat nib across a vertical stroke draws it thin... unless it turns
    // with the stroke, when it lies along it and draws it wide.
    let nib = |follow: bool| {
        let mut b = brush(BrushDynamics {
            tip: TipShape {
                ratio: 0.2,
                follow_stroke: follow,
                ..Default::default()
            },
            ..Default::default()
        });
        let points: Vec<(Vec2, f64)> = (0..=30)
            .map(|i| (Vec2::new(128.0, 20.0 + i as f32 * 3.0), i as f64 * 0.01))
            .collect();
        let (canvas, _) = paint(&mut b, &points, 1, true);
        (0..W).filter(|&x| alpha(&canvas, x, 64) > 127).count()
    };
    let (fixed, follow) = (nib(false), nib(true));
    // Fixed at 0°: the nib's long side is across the vertical stroke.
    assert!(fixed >= 18, "wide across: {fixed}");
    // Following, its long side lies along the stroke: thin across it.
    assert!(follow <= 6, "thin across when following: {follow}");
}

#[test]
fn randomness_is_repeatable_and_bounded() {
    let dynamics = BrushDynamics {
        random: Randomness {
            size: 1.0,
            opacity: 0.5,
            ..Default::default()
        },
        ..Default::default()
    };
    let (a, _) = paint(&mut brush(dynamics), &line(64.0, 0.5), 11, true);
    let (b, _) = paint(&mut brush(dynamics), &line(64.0, 0.5), 11, true);
    let (c, _) = paint(&mut brush(dynamics), &line(64.0, 0.5), 12, true);
    assert_eq!(pixels(&a), pixels(&b), "same seed, same stroke");
    assert_ne!(pixels(&a), pixels(&c), "another seed, another stroke");
    let widest = (30..220).map(|x| thickness(&a, x)).max().unwrap();
    assert!(widest <= 21, "never bigger than the brush: {widest}");
}

#[test]
fn spray_paints_several_dabs_per_step() {
    let spray = |count: u32| {
        let mut b = brush(BrushDynamics {
            random: Randomness {
                count,
                ..Default::default()
            },
            ..Default::default()
        });
        b.brush_options.diameter = 4.0;
        b.brush_options.spacing = 200.0;
        b.jitter = 300.0;
        let (canvas, _) = paint(&mut b, &line(64.0, 0.5), 5, true);
        pixels(&canvas).iter().filter(|&&a| a > 0).count()
    };
    assert!(spray(6) > spray(1) * 3, "{} vs {}", spray(6), spray(1));
}

#[test]
fn undo_puts_back_a_tapered_stroke_exactly() {
    let mut b = brush(BrushDynamics {
        taper: Taper {
            start: 40.0,
            end: 80.0,
            opacity: true,
            ..Default::default()
        },
        tip: TipShape {
            ratio: 0.5,
            follow_stroke: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let (mut canvas, undo) = paint(&mut b, &line(64.0, 0.5), 2, true);
    let before = pixels(&Canvas::new(W, H, Color32::WHITE, 64));
    assert_ne!(pixels(&canvas), before);
    let mut history = History::new();
    history.push_action(undo);
    let mut selection = crate::selection::SelectionManager::new();
    let mut tool = crate::app::tools::Tool::Brush;
    history.undo(&mut canvas, &mut selection, &mut tool);
    assert_eq!(pixels(&canvas), before);
}

#[test]
fn an_image_tip_keeps_its_proportions_and_turns() {
    use crate::brush_engine::brush_options::PixelBrushShape;
    use crate::brush_engine::tip::TipMask;
    let bar = TipMask::from_mask(80, 20, vec![255; 1600]);
    let extent = |angle: f32| {
        let mut b = brush(BrushDynamics {
            tip: TipShape {
                angle,
                ..Default::default()
            },
            ..Default::default()
        });
        b.brush_options.pixel_shape = PixelBrushShape::Custom(bar.clone());
        b.brush_options.diameter = 40.0;
        let (canvas, _) = paint(&mut b, &[(Vec2::new(128.0, 64.0), 0.0)], 1, true);
        let painted: Vec<(usize, usize)> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, y)| alpha(&canvas, x, y) > 127)
            .collect();
        let w = painted.iter().map(|p| p.0).max().unwrap()
            - painted.iter().map(|p| p.0).min().unwrap()
            + 1;
        let h = painted.iter().map(|p| p.1).max().unwrap()
            - painted.iter().map(|p| p.1).min().unwrap()
            + 1;
        (w, h)
    };
    let (w, h) = extent(0.0);
    assert!(
        (39..=41).contains(&w) && (9..=11).contains(&h),
        "a 4:1 bar: {w}×{h}"
    );
    let (w, h) = extent(90.0);
    assert!(
        (9..=11).contains(&w) && (39..=41).contains(&h),
        "turned: {w}×{h}"
    );
}

#[test]
fn a_texture_leaves_grain_and_does_nothing_at_no_strength() {
    use crate::brush_engine::texture::{BrushTexture, TextureMode, builtin};
    let stroke = |texture: Option<BrushTexture>| {
        let mut b = brush(BrushDynamics::default());
        b.brush_options.hardness = 50.0;
        b.texture = texture;
        let (canvas, _) = paint(&mut b, &line(64.0, 0.5), 1, true);
        canvas
    };
    let plain = stroke(None);
    let mut none = BrushTexture::new(builtin()[1].clone());
    none.strength = 0.0;
    assert_eq!(
        pixels(&stroke(Some(none))),
        pixels(&plain),
        "strength 0: unchanged"
    );
    let mut grain = BrushTexture::new(builtin()[1].clone());
    grain.mode = TextureMode::Subtract;
    grain.strength = 1.0;
    let textured = stroke(Some(grain));
    // Along the middle of the stroke, the grain makes the alpha vary.
    let row: Vec<u8> = (40..220).map(|x| alpha(&textured, x, 64)).collect();
    let (lo, hi) = (row.iter().min().unwrap(), row.iter().max().unwrap());
    assert!(hi - lo > 60, "grain: {lo}..{hi}");
    let flat: Vec<u8> = (40..220).map(|x| alpha(&plain, x, 64)).collect();
    assert!(
        flat.iter().all(|&a| a == flat[0]),
        "the plain stroke is even"
    );
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

#[test]
fn the_general_resolve_matches_the_fast_one_in_normal_mode() {
    use crate::canvas::blend::{StrokeColor, resolve_stroke_general, resolve_stroke_normal};
    use crate::canvas::blend_modes::{BlendSpace, LayerBlend};
    let mut rng = 12345u32;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    let original: Vec<Color32> = (0..4096)
        .map(|_| {
            let a = (next() % 256) as u8;
            let c = |v: u32| ((v % 256) as u16 * a as u16 / 255) as u8;
            Color32::from_rgba_premultiplied(c(next()), c(next()), c(next()), a)
        })
        .collect();
    let coverage: Vec<f32> = (0..4096).map(|_| (next() % 1000) as f32 / 999.0).collect();
    let color = StrokeColor::new(Color32::from_rgb(200, 60, 30));
    let mut fast = original.clone();
    let mut general = original.clone();
    resolve_stroke_normal(&original, &coverage, &mut fast, color, 0.8, [3, 7]);
    resolve_stroke_general(
        &original,
        &coverage,
        None,
        &mut general,
        color,
        0.8,
        LayerBlend::Normal,
        BlendSpace::Linear,
        [3, 7],
    );
    for (i, (f, g)) in fast.iter().zip(&general).enumerate() {
        let diff = f
            .to_array()
            .iter()
            .zip(g.to_array())
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap();
        assert!(diff <= 1, "pixel {i}: fast {f:?} general {g:?}");
    }
}

#[test]
fn blend_mode_strokes_follow_their_formulas() {
    use crate::canvas::blend::{color32_to_linear, gamma_color32_to_rgba, gamma_rgba_to_color32};
    use crate::canvas::blend_modes::{BlendSpace, LayerBlend, composite};
    let below = Color32::from_rgb(200, 120, 40);
    let ink = Color32::from_rgb(100, 180, 220);
    for space in [BlendSpace::Gamma, BlendSpace::Linear] {
        for mode in [
            LayerBlend::Multiply,
            LayerBlend::Screen,
            LayerBlend::LinearDodge,
        ] {
            let mut canvas = painted(below);
            canvas.blend_space = space;
            let mut b = Brush::new(30.0, 100.0, ink, 5.0);
            b.brush_options.pressure_size = false;
            b.paint_blend = mode;
            paint_on(&canvas, &mut b, &line(64.0, 0.5), 1);
            let got = pixel(&canvas, 128, 64);
            let expected = if space == BlendSpace::Gamma {
                let r = composite(
                    mode,
                    gamma_color32_to_rgba(ink),
                    gamma_color32_to_rgba(below),
                    0.0,
                );
                gamma_rgba_to_color32(r)
            } else {
                let r = composite(mode, color32_to_linear(ink), color32_to_linear(below), 0.0);
                crate::canvas::blend::rgba_to_color32_fast(r)
            };
            let diff = got
                .to_array()
                .iter()
                .zip(expected.to_array())
                .map(|(a, b)| a.abs_diff(b))
                .max()
                .unwrap();
            assert!(diff <= 1, "{mode:?} {space:?}: {got:?} vs {expected:?}");
        }
    }
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

#[test]
fn colour_randomness_varies_the_colour_and_repeats_with_its_seed() {
    let run = |seed: u64| {
        let canvas = painted(Color32::TRANSPARENT);
        let mut b = brush(hue_random());
        b.brush_options.color = Color32::from_rgb(220, 40, 40);
        b.brush_options.spacing = 60.0;
        paint_on(&canvas, &mut b, &line(64.0, 0.5), seed);
        (40..220).map(|x| pixel(&canvas, x, 64)).collect::<Vec<_>>()
    };
    let (a, b, c) = (run(4), run(4), run(5));
    assert_eq!(a, b, "same seed, same colours");
    assert_ne!(a, c, "another seed, other colours");
    let greens = a.iter().filter(|p| p.g() > p.r()).count();
    assert!(greens > 5, "some dabs turned towards green: {greens}");
}

#[test]
fn a_coloured_tail_leaves_nothing_behind() {
    let run = |end: f32| {
        let canvas = painted(Color32::TRANSPARENT);
        let mut d = hue_random();
        d.taper = Taper {
            end,
            ..Default::default()
        };
        let mut b = brush(d);
        b.brush_options.color = Color32::from_rgb(220, 40, 40);
        paint_on(&canvas, &mut b, &line(64.0, 0.5), 9);
        canvas
    };
    let (tapered, plain) = (run(60.0), run(0.0));
    for y in 40..90 {
        for x in 0..150 {
            assert_eq!(pixel(&tapered, x, y), pixel(&plain, x, y), "({x}, {y})");
        }
    }
}

#[test]
fn undo_puts_back_a_blend_mode_stroke_exactly() {
    use crate::canvas::blend_modes::LayerBlend;
    let below = Color32::from_rgb(90, 160, 30);
    let mut canvas = painted(below);
    let mut b = brush(hue_random());
    b.paint_blend = LayerBlend::Overlay;
    let undo = paint_on(&canvas, &mut b, &line(64.0, 0.5), 3);
    assert_ne!(pixel(&canvas, 128, 64), below);
    let mut history = History::new();
    history.push_action(undo);
    let mut selection = crate::selection::SelectionManager::new();
    let mut tool = crate::app::tools::Tool::Brush;
    history.undo(&mut canvas, &mut selection, &mut tool);
    for x in (0..W).step_by(7) {
        assert_eq!(pixel(&canvas, x, 64), below);
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

#[test]
fn a_leaning_pen_widens_the_stroke_when_tilt_drives_size() {
    use crate::brush_engine::dynamics::{PenTilt, TiltDynamics};
    let mut b = brush(BrushDynamics {
        tilt: TiltDynamics {
            size: 1.0,
            opacity: 0.0,
        },
        ..Default::default()
    });
    b.brush_options.diameter = 12.0;
    let upright = paint_pen(
        &mut b,
        &line(64.0, 0.5),
        1.0,
        Some(PenTilt {
            lean: 0.0,
            direction: 0.0,
        }),
    );
    let flat = paint_pen(
        &mut b,
        &line(64.0, 0.5),
        1.0,
        Some(PenTilt {
            lean: 1.0,
            direction: 0.0,
        }),
    );
    let none = paint_pen(&mut b, &line(64.0, 0.5), 1.0, None);
    let (u, f, n) = (
        thickness(&upright, 128),
        thickness(&flat, 128),
        thickness(&none, 128),
    );
    assert!(f >= u + 8, "flat {f} vs upright {u}");
    assert_eq!(u, n, "no tilt reported: as upright");
}

#[test]
fn a_nib_that_follows_the_pen_turns_with_its_lean() {
    use crate::brush_engine::dynamics::PenTilt;
    let mut b = brush(BrushDynamics {
        tip: TipShape {
            ratio: 0.2,
            follow_tilt: true,
            ..Default::default()
        },
        ..Default::default()
    });
    b.brush_options.diameter = 40.0;
    let extent = |direction: f32| {
        let canvas = paint_pen(
            &mut b.clone(),
            &[(Vec2::new(128.0, 64.0), 0.0)],
            1.0,
            Some(PenTilt {
                lean: 0.5,
                direction,
            }),
        );
        let painted: Vec<(usize, usize)> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, y)| alpha(&canvas, x, y) > 127)
            .collect();
        let w =
            painted.iter().map(|p| p.0).max().unwrap() - painted.iter().map(|p| p.0).min().unwrap();
        let h =
            painted.iter().map(|p| p.1).max().unwrap() - painted.iter().map(|p| p.1).min().unwrap();
        (w, h)
    };
    let (w, h) = extent(0.0);
    assert!(w > h * 3, "leaning right: long across, {w}×{h}");
    let (w, h) = extent(std::f32::consts::FRAC_PI_2);
    assert!(h > w * 3, "leaning up: long up and down, {w}×{h}");
}

#[test]
fn pressure_curves_shape_the_response() {
    use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
    let at = |curve: Option<SoftnessCurve>| {
        let mut b = brush(BrushDynamics::default());
        b.brush_options.pressure_size = true;
        b.brush_options.pressure_curves.size = curve;
        paint_pen(&mut b, &line(64.0, 0.5), 0.3, None)
    };
    let points = |p: &[(f32, f32)]| SoftnessCurve {
        points: p.iter().map(|&(x, y)| CurvePoint::new(x, y)).collect(),
    };
    let straight = at(None);
    assert_eq!(
        pixels(&at(Some(points(&[(0.0, 0.0), (1.0, 1.0)])))),
        pixels(&straight),
        "a linear curve is no curve"
    );
    let firm = at(Some(points(&[(0.0, 0.0), (0.3, 0.8), (1.0, 1.0)])));
    assert!(
        thickness(&firm, 128) > thickness(&straight, 128) + 6,
        "firm: {} vs {}",
        thickness(&firm, 128),
        thickness(&straight, 128)
    );
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

#[test]
fn every_preset_paints_and_undoes_exactly() {
    use crate::brush_engine::brush_options::BlendMode;
    let presets = crate::PainterApp::create_default_brush_presets(Color32::BLACK);
    assert!(presets.len() >= 14);
    for preset in presets {
        let mut brush = preset.brush.clone();
        let eraser = brush.brush_options.blend_mode == BlendMode::Eraser;
        // Erasers need paint to erase; the rest paint on a light layer.
        let below = if eraser {
            Color32::from_rgb(40, 90, 160)
        } else {
            Color32::from_rgb(235, 230, 220)
        };
        let before = pixels_rgba(&painted(below));
        let (mut canvas, undo) = paint_preset(&mut brush, below);
        let after = pixels_rgba(&canvas);
        let changed = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        // A 1 px pixel-art line changes about as many pixels as it's long.
        assert!(
            changed > 150,
            "{}: only {changed} pixels changed",
            preset.name
        );
        let mut history = History::new();
        history.push_action(undo);
        let mut selection = crate::selection::SelectionManager::new();
        let mut tool = crate::app::tools::Tool::Brush;
        history.undo(&mut canvas, &mut selection, &mut tool);
        assert!(
            pixels_rgba(&canvas) == before,
            "{}: undo isn't exact",
            preset.name
        );
    }
}

fn pixels_rgba(canvas: &Canvas) -> Vec<Color32> {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .map(|(x, y)| pixel(canvas, x, y))
        .collect()
}

#[test]
#[ignore = "timing; run with --release --ignored --nocapture"]
fn preset_stroke_times() {
    for preset in crate::PainterApp::create_default_brush_presets(Color32::BLACK) {
        let mut brush = preset.brush.clone();
        let start = std::time::Instant::now();
        for _ in 0..5 {
            paint_preset(&mut brush, Color32::WHITE);
        }
        eprintln!(
            "{:<18} {:>7.2} ms",
            preset.name,
            start.elapsed().as_secs_f64() * 200.0
        );
    }
}

#[test]
fn speed_stays_smooth_when_samples_come_in_bursts() {
    // A mouse at a steady speed: four moves per frame stamped microseconds
    // apart, a frame every 4 ms. The width must stay steady, and match
    // evenly timed samples at the same speed.
    let dynamics = BrushDynamics {
        speed: SpeedDynamics {
            size: -0.8,
            opacity: 0.0,
        },
        ..Default::default()
    };
    let speed = 1200.0; // points per second
    let bursty: Vec<(Vec2, f64)> = (0..240)
        .map(|i| {
            let frame = (i / 4) as f64 * 0.004;
            let t = frame + (i % 4) as f64 * 1e-6;
            // Positions move on at the real hand speed between events.
            let x = 20.0 + (frame + (i % 4) as f64 * 0.001) * speed;
            (Vec2::new(x as f32, 64.0), t)
        })
        .collect();
    let even: Vec<(Vec2, f64)> = (0..240)
        .map(|i| {
            let t = i as f64 * 0.001;
            (Vec2::new((20.0 + t * speed) as f32, 64.0), t)
        })
        .collect();
    let (bursty, _) = paint(&mut brush(dynamics), &bursty, 1, true);
    let (even, _) = paint(&mut brush(dynamics), &even, 1, true);
    let widths: Vec<usize> = (80..220).map(|x| thickness(&bursty, x)).collect();
    let (lo, hi) = (*widths.iter().min().unwrap(), *widths.iter().max().unwrap());
    assert!(hi - lo <= 2, "steady width: {lo}..{hi}");
    let reference = thickness(&even, 150) as i32;
    assert!(
        (thickness(&bursty, 150) as i32 - reference).abs() <= 1,
        "same as evenly timed: {} vs {reference}",
        thickness(&bursty, 150)
    );
}

/// Renders every built-in tip and preset stroke to PNGs in
/// `RUSTY_PAINTER_RENDER_DIR`, for looking at them.
#[test]
#[ignore = "writes images; set RUSTY_PAINTER_RENDER_DIR"]
fn render_presets_and_tips() {
    let Ok(dir) = std::env::var("RUSTY_PAINTER_RENDER_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&dir);
    for p in crate::brush_engine::texture::builtin() {
        let n = p.size as u32;
        let img = image::GrayImage::from_fn(n * 2, n * 2, |x, y| {
            image::Luma([(p.data[((y % n) * n + x % n) as usize] * 255.0) as u8])
        });
        img.save(dir.join(format!("paper_{}.png", p.name))).unwrap();
    }
    for (name, tip) in crate::brush_engine::tip::builtin() {
        let img = image::GrayImage::from_raw(
            tip.width as u32,
            tip.height as u32,
            tip.pixels.iter().map(|v| 255 - v).collect(),
        )
        .unwrap();
        img.save(dir.join(format!("tip_{name}.png"))).unwrap();
    }
    for preset in crate::PainterApp::create_default_brush_presets(Color32::BLACK) {
        let mut brush = preset.brush.clone();
        let eraser =
            brush.brush_options.blend_mode == crate::brush_engine::brush_options::BlendMode::Eraser;
        let below = if eraser {
            Color32::from_rgb(40, 90, 160)
        } else {
            Color32::WHITE
        };
        let (canvas, _) = paint_preset(&mut brush, below);
        let mut img = image::RgbaImage::new(W as u32, H as u32);
        for y in 0..H {
            for x in 0..W {
                let [r, g, b, a] = pixel(&canvas, x, y).to_srgba_unmultiplied();
                // Over white, as it would look on paper.
                let over = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
                img.put_pixel(
                    x as u32,
                    y as u32,
                    image::Rgba([over(r), over(g), over(b), 255]),
                );
            }
        }
        img.save(dir.join(format!(
            "preset_{}.png",
            preset.name.replace([' ', '(', ')'], "_")
        )))
        .unwrap();
    }
}

#[test]
fn every_preset_paints_the_same_after_a_trip_through_a_preset_file() {
    let presets = crate::PainterApp::create_default_brush_presets(Color32::BLACK);
    let bytes = crate::brush_engine::preset_file::encode(&presets).unwrap();
    let back = crate::brush_engine::preset_file::decode(&bytes).unwrap();
    assert_eq!(back.len(), presets.len());
    let below = Color32::from_rgb(235, 230, 220);
    for (a, b) in presets.iter().zip(&back) {
        let (ca, _) = paint_preset(&mut a.brush.clone(), below);
        let (cb, _) = paint_preset(&mut b.brush.clone(), below);
        assert!(
            pixels_rgba(&ca) == pixels_rgba(&cb),
            "{} paints differently after saving",
            a.name
        );
    }
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

#[test]
fn without_an_airbrush_holding_still_adds_nothing() {
    let (canvas, centre) = hold_airbrush(&mut airbrush(0.0), 0.5, 1);
    assert!(centre.windows(2).all(|w| w[0] == w[1]));
    let (plain, _) = paint(
        &mut airbrush(0.0),
        &[(Vec2::new(128.0, 64.0), 0.0)],
        1,
        true,
    );
    assert!(pixels(&canvas) == pixels(&plain));
}

#[test]
fn an_airbrush_builds_up_while_held_still_at_its_rate() {
    let (_, centre) = hold_airbrush(&mut airbrush(20.0), 1.0, 1);
    let first = centre[0];
    let last = *centre.last().unwrap();
    assert!(centre.windows(2).all(|w| w[1] >= w[0]), "only builds up");
    assert!(last > first + 100, "{first} → {last}");
    // 20 dabs a second: about 20 increases in a second.
    let steps = centre.windows(2).filter(|w| w[1] > w[0]).count();
    assert!((17..=22).contains(&steps), "{steps} dabs");
    // A faster rate builds up sooner.
    let (_, fast) = hold_airbrush(&mut airbrush(60.0), 0.3, 1);
    let (_, slow) = hold_airbrush(&mut airbrush(20.0), 0.3, 1);
    assert!(fast.last() > slow.last());
}

#[test]
fn an_airbrush_is_repeatable_and_undoes_exactly() {
    let mut b = airbrush(30.0);
    b.jitter = 20.0;
    b.dynamics.random.size = 0.5;
    let (a, _) = hold_airbrush(&mut b.clone(), 0.5, 7);
    let (c, _) = hold_airbrush(&mut b.clone(), 0.5, 7);
    assert!(pixels(&a) == pixels(&c));

    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let before = pixels(&canvas);
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(3);
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        stroke.add_sample(&mut b, Vec2::new(60.0, 60.0), 1.0, Some(0.0), &mut ctx);
        stroke.airbrush(&mut b, 0.5, &mut ctx);
        stroke.add_sample(&mut b, Vec2::new(150.0, 70.0), 1.0, Some(0.6), &mut ctx);
        stroke.airbrush(&mut b, 1.0, &mut ctx);
        stroke.finish(&mut b, &mut ctx);
    }
    assert!(pixels(&canvas) != before);
    let mut history = History::new();
    history.push_action(undo);
    let mut selection = crate::selection::SelectionManager::new();
    let mut tool = crate::app::tools::Tool::Brush;
    history.undo(&mut canvas, &mut selection, &mut tool);
    assert!(pixels(&canvas) == before);
}

#[test]
fn a_long_stall_does_not_bank_airbrush_dabs() {
    // Ten seconds late: at most a handful of dabs at once, not 200.
    let mut b = airbrush(20.0);
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(3);
    let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
    stroke.add_sample(&mut b, Vec2::new(128.0, 64.0), 1.0, Some(0.0), &mut ctx);
    let start = alpha(&canvas, 128, 64);
    stroke.airbrush(&mut b, 10.0, &mut ctx);
    let after = alpha(&canvas, 128, 64);
    // The next call a moment later paints nothing more.
    stroke.airbrush(&mut b, 10.01, &mut ctx);
    assert_eq!(alpha(&canvas, 128, 64), after);
    // Each 5% dab adds about 13 levels near white-on-black; 8 at most.
    assert!(after > start && after - start < 8 * 16, "{start} → {after}");
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

#[test]
fn several_tips_are_taken_in_turn() {
    use crate::brush_engine::brush_options::TipOrder;
    let used = tips_used(
        &mut multi_tip(TipOrder::Sequence),
        &along_x(40.0, 200.0, 20),
        1,
    );
    assert!(used.len() >= 5);
    for (i, &t) in used.iter().enumerate() {
        assert_eq!(t as usize, i % 3, "{used:?}");
    }
}

#[test]
fn random_tips_use_every_tip_and_repeat_with_the_seed() {
    use crate::brush_engine::brush_options::TipOrder;
    let mut b = multi_tip(TipOrder::Random);
    b.brush_options.spacing = 10.0;
    let a = tips_used(&mut b.clone(), &along_x(20.0, 230.0, 30), 5);
    let c = tips_used(&mut b.clone(), &along_x(20.0, 230.0, 30), 5);
    assert_eq!(a, c);
    for k in 0..3 {
        assert!(a.contains(&k), "tip {k} never used: {a:?}");
    }
}

#[test]
fn pressure_and_direction_pick_the_tip() {
    use crate::brush_engine::brush_options::TipOrder;
    let mut b = multi_tip(TipOrder::Pressure);
    let light: Vec<_> = along_x(40.0, 200.0, 10)
        .into_iter()
        .map(|(p, _)| (p, 0.1))
        .collect();
    let full: Vec<_> = along_x(40.0, 200.0, 10);
    assert!(tips_used(&mut b.clone(), &light, 1).iter().all(|&t| t == 0));
    assert!(tips_used(&mut b, &full, 1).iter().all(|&t| t == 2));

    let mut b = multi_tip(TipOrder::Direction);
    // Rightwards is 0°: the first tip; leftwards 180°: the middle one of
    // three (120°..240°).
    let right = tips_used(&mut b.clone(), &along_x(40.0, 200.0, 10), 1);
    let left = tips_used(&mut b, &along_x(200.0, 40.0, 10), 1);
    assert!(right[1..].iter().all(|&t| t == 0), "{right:?}");
    assert!(left[1..].iter().all(|&t| t == 1), "{left:?}");
}

#[test]
fn each_dab_paints_with_its_own_tip() {
    use crate::brush_engine::brush_options::TipOrder;
    // Three dabs 50 px apart, in turn: the marks sit left, centre, right
    // of each dab's centre.
    let mut b = multi_tip(TipOrder::Sequence);
    b.brush_options.spacing = 50.0 / 32.0 * 100.0;
    let points = [(Vec2::new(60.0, 64.0), 0.0), (Vec2::new(160.0, 64.0), 0.1)];
    let (canvas, _) = paint(&mut b, &points, 1, true);
    for (k, centre) in [60, 110, 160].into_iter().enumerate() {
        let mark = centre - 10 + k as i32 * 10;
        assert!(
            alpha(&canvas, mark as usize, 64) > 200,
            "dab {k}: no mark at {mark}"
        );
        for other in (0..3).filter(|&j| j != k) {
            let x = centre - 10 + other as i32 * 10;
            assert!(alpha(&canvas, x as usize, 64) < 30, "dab {k}: mark at {x}");
        }
    }
}

#[test]
fn a_set_of_the_same_tip_paints_like_the_tip_alone() {
    use crate::brush_engine::brush_options::{PixelBrushShape, TipOrder};
    let tip = marker_tips().remove(1);
    let mut one = Brush::new(32.0, 100.0, Color32::BLACK, 20.0);
    one.brush_options.pixel_shape = PixelBrushShape::Custom(tip.clone());
    let mut two = one.clone();
    two.brush_options.extra_tips = vec![tip.clone(), tip];
    two.brush_options.tip_order = TipOrder::Random;
    let (a, _) = paint(&mut one, &line(64.0, 0.5), 1, true);
    let (b, _) = paint(&mut two, &line(64.0, 0.5), 1, true);
    assert!(pixels(&a) == pixels(&b));
}

fn dual_brush(dual: Option<crate::brush_engine::dual::DualTip>) -> Brush {
    let mut b = Brush::new(20.0, 60.0, Color32::BLACK, 10.0);
    b.brush_options.pressure_size = false;
    b.dual = dual;
    b
}

#[test]
fn a_dual_tip_covering_everything_changes_nothing() {
    use crate::brush_engine::brush_options::PixelBrushShape;
    use crate::brush_engine::dual::DualTip;
    let full = DualTip {
        shape: PixelBrushShape::Square,
        size: 4.0,
        hardness: 100.0,
        spacing: 5.0,
        scatter: 0.0,
        random_angle: false,
        ..Default::default()
    };
    let (plain, _) = paint(&mut dual_brush(None), &line(64.0, 0.5), 1, true);
    let (masked, _) = paint(&mut dual_brush(Some(full)), &line(64.0, 0.5), 1, true);
    assert!(pixels(&plain) == pixels(&masked));
}

#[test]
fn an_empty_dual_tip_masks_all_the_paint() {
    use crate::brush_engine::brush_options::PixelBrushShape;
    use crate::brush_engine::dual::{DualMode, DualTip};
    let blank = crate::brush_engine::tip::TipMask::from_mask(8, 8, vec![0; 64]);
    for mode in DualMode::ALL {
        let dual = DualTip {
            shape: PixelBrushShape::Custom(blank.clone()),
            mode,
            ..Default::default()
        };
        let (canvas, _) = paint(&mut dual_brush(Some(dual)), &line(64.0, 0.5), 1, true);
        assert!(pixels(&canvas).iter().all(|&a| a == 0), "{mode:?}");
    }
}

#[test]
fn a_spatter_dual_tip_breaks_the_stroke_up_and_undoes_exactly() {
    use crate::brush_engine::dual::DualTip;
    let spatter = crate::brush_engine::tip::builtin()
        .iter()
        .find(|(n, _)| *n == "Spatter")
        .map(|(_, t)| t.clone())
        .unwrap();
    let dual = DualTip {
        shape: crate::brush_engine::brush_options::PixelBrushShape::Custom(spatter),
        size: 0.6,
        ..Default::default()
    };
    let (plain, _) = paint(&mut dual_brush(None), &line(64.0, 0.5), 1, true);
    let (mut masked, undo) = paint(&mut dual_brush(Some(dual)), &line(64.0, 0.5), 1, true);
    let painted = |c: &Canvas| pixels(c).iter().filter(|&&a| a > 20).count();
    let (full, broken) = (painted(&plain), painted(&masked));
    assert!(
        broken > full / 10 && broken < full * 3 / 4,
        "{broken} of {full}"
    );
    // Nowhere more paint than the plain stroke.
    assert!(
        pixels(&plain)
            .iter()
            .zip(pixels(&masked))
            .all(|(p, m)| m <= *p)
    );
    let mut history = History::new();
    history.push_action(undo);
    let mut selection = crate::selection::SelectionManager::new();
    let mut tool = crate::app::tools::Tool::Brush;
    history.undo(&mut masked, &mut selection, &mut tool);
    assert!(pixels(&masked).iter().all(|&a| a == 0));
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

#[test]
fn wet_edges_thin_the_middle_of_a_wash_and_keep_its_rim() {
    // A thick line across tile borders (tiles are 64 px): 50 px wide.
    let (plain, _) = paint(&mut wash(0.0), &line(64.0, 0.5), 1, true);
    let (mut wet, undo) = paint(&mut wash(0.6), &line(64.0, 0.5), 1, true);
    let (middle, rim) = (alpha(&wet, 128, 64), alpha(&wet, 128, 64 - 22));
    let plain_middle = alpha(&plain, 128, 64) as u32;
    assert!(
        (middle as u32) < plain_middle * 2 / 3,
        "middle {middle} vs {plain_middle}"
    );
    assert!(rim > middle + 40, "rim {rim}, middle {middle}");
    // Across the tile border at x = 128 as elsewhere: no seam.
    let (left, right) = (alpha(&wet, 127, 64), alpha(&wet, 128, 64));
    assert!(left.abs_diff(right) <= 1, "{left} | {right}");
    // Nothing outside the stroke, and undo restores the layer exactly.
    for (p, w) in pixels(&plain).iter().zip(pixels(&wet)) {
        assert!(*p > 0 || w == 0);
    }
    let mut history = History::new();
    history.push_action(undo);
    let mut selection = crate::selection::SelectionManager::new();
    let mut tool = crate::app::tools::Tool::Brush;
    history.undo(&mut wet, &mut selection, &mut tool);
    assert!(pixels(&wet).iter().all(|&a| a == 0));
}

#[test]
fn without_wet_edges_the_stroke_is_unchanged() {
    let mut off = wash(0.0);
    off.wet_edge_width = 20.0;
    let (a, _) = paint(&mut wash(0.0), &line(64.0, 0.5), 1, true);
    let (b, _) = paint(&mut off, &line(64.0, 0.5), 1, true);
    assert!(pixels(&a) == pixels(&b));
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

#[test]
fn a_bristle_brush_paints_one_streak_per_hair_across_the_stroke() {
    let canvas = stroke_with_pressure(&mut bristle_brush(0.0), &along_x(20.0, 236.0, 40));
    assert_eq!(
        runs(&canvas, 128, true),
        8,
        "streaks across a horizontal stroke"
    );
    // Along the stroke each streak is continuous.
    let streak_row = (0..H).find(|&y| alpha(&canvas, 128, y) > 60).unwrap();
    assert_eq!(runs(&canvas, streak_row, false), 1);
    // Going down, the hairs lie across it: the streaks are side by side.
    let down: Vec<(Vec2, f32)> = (0..=30)
        .map(|i| (Vec2::new(128.0, 10.0 + i as f32 * 3.6), 1.0))
        .collect();
    let canvas = stroke_with_pressure(&mut bristle_brush(0.0), &down);
    assert_eq!(
        runs(&canvas, 64, false),
        8,
        "streaks across a vertical stroke"
    );
}

#[test]
fn bristles_fan_out_with_pressure_and_run_dry() {
    let width = |c: &Canvas| {
        (0..H).rfind(|&y| alpha(c, 128, y) > 30).unwrap()
            - (0..H).find(|&y| alpha(c, 128, y) > 30).unwrap()
    };
    let light: Vec<_> = along_x(20.0, 236.0, 40)
        .into_iter()
        .map(|(p, _)| (p, 0.2))
        .collect();
    let soft = stroke_with_pressure(&mut bristle_brush(0.0), &light);
    let hard = stroke_with_pressure(&mut bristle_brush(0.0), &along_x(20.0, 236.0, 40));
    assert!(
        width(&hard) > width(&soft) * 2,
        "{} vs {}",
        width(&hard),
        width(&soft)
    );

    let dry = stroke_with_pressure(&mut bristle_brush(100.0), &along_x(20.0, 236.0, 40));
    let column = |x: usize| (0..H).filter(|&y| alpha(&dry, x, y) > 30).count();
    assert!(
        column(40) > 0 && column(220) == 0,
        "{} → {}",
        column(40),
        column(220)
    );
    assert!(column(120) < column(40));
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

#[test]
fn a_colour_tip_paints_its_own_colours_when_asked() {
    use crate::brush_engine::brush_options::PixelBrushShape;
    let mut b = Brush::new(60.0, 90.0, Color32::BLACK, 100.0);
    b.brush_options.pressure_size = false;
    b.brush_options.pixel_shape = PixelBrushShape::Custom(builtin_tip("Flower"));
    let dab = [(Vec2::new(64.0, 64.0), 0.0)];
    let (grey, _) = paint(&mut b.clone(), &dab, 1, true);
    b.brush_options.tip_colors = true;
    let (colour, _) = paint(&mut b, &dab, 1, true);
    // The heart is yellow, a petal pink; without the option, black.
    let heart = rgb(&colour, 64, 64);
    assert!(
        heart[0] > 200 && heart[1] > 150 && heart[2] < 120,
        "{heart:?}"
    );
    let petal = rgb(&colour, 64 + 18, 64);
    assert!(petal[0] > 200 && petal[1] < 170, "{petal:?}");
    assert_eq!(rgb(&grey, 64, 64), [0, 0, 0]);
    // The same shape either way.
    for y in 30..100 {
        for x in 30..100 {
            assert!(alpha(&grey, x, y).abs_diff(alpha(&colour, x, y)) <= 1);
        }
    }
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

#[test]
fn a_ribbon_lays_its_picture_along_the_stroke_and_repeats_it() {
    // 32 px wide.
    let (canvas, _) = paint(
        &mut ribbon_brush("Striped ribbon", true),
        &along_x(0.0, 256.0, 40)
            .into_iter()
            .enumerate()
            .map(|(i, (p, _))| (p, i as f64 * 0.01))
            .collect::<Vec<_>>(),
        1,
        true,
    );
    // As wide as the brush, centred on the stroke.
    let across = (0..H).filter(|&y| alpha(&canvas, 100, y) > 128).count();
    assert!((26..=32).contains(&across), "{across}");
    // Red and white stripes along it, 16 px each way.
    let reds: Vec<bool> = (8..120).map(|x| rgb(&canvas, x, 64)[1] < 120).collect();
    let changes = reds.windows(2).filter(|w| w[0] != w[1]).count();
    assert!((12..=15).contains(&changes), "{changes} stripe edges");
    // The picture repeats every picture length: its width over its height,
    // times the ribbon's width.
    let tip = builtin_tip("Striped ribbon");
    let length = (tip.width as f32 / tip.height as f32 * 32.0).round() as usize;
    // (Up to a pixel off at the stripes' edges: the length isn't whole.)
    let differ = (20..100)
        .filter(|&x| {
            let (a, b) = (rgb(&canvas, x, 60), rgb(&canvas, x + length, 60));
            a.iter().zip(b).any(|(p, q)| p.abs_diff(q) > 40)
        })
        .count();
    assert!(differ <= 10, "{differ} of 80 differ");
}

#[test]
fn a_ribbon_follows_a_bend_and_undoes_exactly() {
    let bend: Vec<(Vec2, f64)> = (0..=40)
        .map(|i| {
            let t = i as f32 / 40.0 * std::f32::consts::PI;
            (
                Vec2::new(128.0 - 80.0 * t.cos(), 110.0 - 70.0 * t.sin()),
                i as f64 * 0.01,
            )
        })
        .collect();
    let (mut canvas, undo) = paint(&mut ribbon_brush("Lace", false), &bend, 1, true);
    // Lace all along the arch, nothing at its centre.
    let painted = |x: usize, y: usize| {
        (y.saturating_sub(18)..(y + 18).min(H)).any(|yy| {
            (x.saturating_sub(18)..(x + 18).min(W)).any(|xx| alpha(&canvas, xx, yy) > 100)
        })
    };
    assert!(painted(48, 110) && painted(128, 40) && painted(208, 110));
    assert!(!painted(128, 105));
    let mut history = History::new();
    history.push_action(undo);
    let mut selection = crate::selection::SelectionManager::new();
    let mut tool = crate::app::tools::Tool::Brush;
    history.undo(&mut canvas, &mut selection, &mut tool);
    assert!(pixels(&canvas).iter().all(|&a| a == 0));
}

#[test]
fn a_mirrored_ribbon_paints_its_mirror_image() {
    use crate::brush_engine::symmetry::{Symmetry, SymmetryMode};
    let symmetry = Symmetry {
        mode: SymmetryMode::Vertical,
        center: Vec2::new(128.0, 64.0),
        ..Default::default()
    };
    let copies = symmetry.copies();
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(1);
    let mut brush = ribbon_brush("Lace", false);
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles)
            .with_symmetry(&symmetry, &copies);
        for (i, (p, _)) in along_x(10.0, 100.0, 20).into_iter().enumerate() {
            let p = Vec2::new(p.x, 50.0);
            stroke.add_sample(&mut brush, p, 1.0, Some(i as f64 * 0.01), &mut ctx);
        }
        stroke.finish(&mut brush, &mut ctx);
    }
    // Left and right halves mirror each other (to within a pixel's edge).
    let differ = (0..H)
        .flat_map(|y| (20..100).map(move |x| (x, y)))
        .filter(|&(x, y)| alpha(&canvas, x, y).abs_diff(alpha(&canvas, 255 - x, y)) > 60)
        .count();
    let painted = (0..H)
        .flat_map(|y| (20..100).map(move |x| (x, y)))
        .filter(|&(x, y)| alpha(&canvas, x, y) > 60)
        .count();
    assert!(
        painted > 400 && differ < painted / 20,
        "{differ} of {painted}"
    );
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

#[test]
fn hatching_paints_lines_pinned_to_the_canvas() {
    let light: Vec<(Vec2, f32)> = along_x(20.0, 236.0, 30)
        .into_iter()
        .map(|(p, _)| (p, 0.2))
        .collect();
    let canvas = stroke_with_pressure(&mut hatch_brush(), &light);
    // Horizontal lines, 6 px apart, down the middle of the stroke.
    let lines = runs(&canvas, 128, true);
    assert!((5..=8).contains(&lines), "{lines} lines");
    let column = |c: &Canvas, x: usize| (0..H).map(|y| alpha(c, x, y) > 100).collect::<Vec<_>>();
    assert_eq!(
        column(&canvas, 100),
        column(&canvas, 160),
        "the same lines all along"
    );
    // A second stroke a little lower: its lines fall on the same rows.
    let lower: Vec<(Vec2, f32)> = light
        .iter()
        .map(|&(p, q)| (p + Vec2::new(0.0, 9.0), q))
        .collect();
    let other = stroke_with_pressure(&mut hatch_brush(), &lower);
    // (Where both strokes cover fully, away from their soft edges.)
    for y in 60..78 {
        if alpha(&canvas, 128, y) > 100 && alpha(&other, 128, y) > 0 {
            assert!(alpha(&other, 128, y) > 100, "row {y} off the shared lines");
        }
    }
    // Pressing harder cross-hatches: more ink.
    let hard = stroke_with_pressure(&mut hatch_brush(), &along_x(20.0, 236.0, 30));
    let ink = |c: &Canvas| pixels(c).iter().map(|&a| a as u32).sum::<u32>();
    assert!(ink(&hard) > ink(&canvas) * 3 / 2);
}

#[test]
fn a_sketch_brush_webs_between_passes() {
    let zigzag: Vec<(Vec2, f32)> = (0..=60)
        .map(|i| {
            let t = i as f32 / 60.0;
            let x = 40.0 + 170.0 * t;
            let y = if i % 20 < 10 {
                40.0 + (i % 10) as f32 * 5.0
            } else {
                90.0 - (i % 10) as f32 * 5.0
            };
            (Vec2::new(x, y), 1.0)
        })
        .collect();
    let mut sketch = Brush::new(2.0, 90.0, Color32::BLACK, 20.0);
    sketch.brush_options.pressure_size = false;
    sketch.brush_type = crate::brush_engine::brush::BrushType::Sketch;
    sketch.sketch.density = 0.3;
    let mut plain = sketch.clone();
    plain.brush_type = crate::brush_engine::brush::BrushType::Soft;
    let webbed = stroke_with_pressure(&mut sketch.clone(), &zigzag);
    let line = stroke_with_pressure(&mut plain, &zigzag);
    let painted = |c: &Canvas| pixels(c).iter().filter(|&&a| a > 10).count();
    assert!(
        painted(&webbed) > painted(&line) * 2,
        "{} vs {}",
        painted(&webbed),
        painted(&line)
    );
    // Its own line is all there.
    let missing = pixels(&line)
        .iter()
        .zip(pixels(&webbed))
        .filter(|&(&p, w)| p > 100 && w < p / 2)
        .count();
    assert_eq!(missing, 0);
    // Repeatable with the same seed.
    assert!(pixels(&stroke_with_pressure(&mut sketch.clone(), &zigzag)) == pixels(&webbed));
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

#[test]
fn with_wrap_around_a_dab_past_an_edge_comes_in_at_the_other() {
    let (w, h) = (W as f32, H as f32);
    // Across the right edge: the same as its two halves painted plainly.
    let wrapped = dabs_at(&[Vec2::new(w - 5.0, 40.0)], true);
    let halves = dabs_at(&[Vec2::new(w - 5.0, 40.0), Vec2::new(-5.0, 40.0)], false);
    assert!(pixels(&wrapped) == pixels(&halves));
    assert!(alpha(&wrapped, 3, 40) > 200, "came in on the left");
    // A corner reaches all four corners.
    let corner = dabs_at(&[Vec2::new(2.0, 2.0)], true);
    for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
        assert!(alpha(&corner, x, y) > 100, "({x}, {y})");
    }
    // Far off the canvas: whole canvases away, the same dab.
    let far = dabs_at(&[Vec2::new(2.0 * w + 50.0, -3.0 * h + 60.0)], true);
    let home = dabs_at(&[Vec2::new(50.0, 60.0)], true);
    assert!(pixels(&far) == pixels(&home));
    // Without wrap-around nothing comes round.
    let plain = dabs_at(&[Vec2::new(w - 5.0, 40.0)], false);
    assert_eq!(alpha(&plain, 3, 40), 0);
}

fn mapped(mappings: Vec<crate::brush_engine::dynamics::InputMapping>) -> Brush {
    let mut b = brush(BrushDynamics::default());
    b.inputs = mappings;
    b
}

#[test]
fn a_distance_mapping_fades_the_size_in() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let mut b = mapped(vec![InputMapping {
        sensor: Sensor::Distance,
        setting: DabSetting::Size,
        amount: 1.0,
        length: 150.0,
        ..Default::default()
    }]);
    let (canvas, _) = paint(&mut b, &line(64.0, 0.5), 1, true);
    let (early, late) = (thickness(&canvas, 40), thickness(&canvas, 220));
    assert!(
        early < late / 2,
        "thin at first, full later: {early} vs {late}"
    );
}

#[test]
fn a_negative_amount_works_the_other_way() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let mut b = mapped(vec![InputMapping {
        sensor: Sensor::Distance,
        setting: DabSetting::Size,
        amount: -0.8,
        length: 200.0,
        ..Default::default()
    }]);
    let (canvas, _) = paint(&mut b, &line(64.0, 0.5), 1, true);
    assert!(thickness(&canvas, 40) > thickness(&canvas, 220));
}

#[test]
fn random_per_stroke_is_one_value_for_the_whole_stroke() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let mapping = |sensor| InputMapping {
        sensor,
        setting: DabSetting::Opacity,
        amount: 1.0,
        ..Default::default()
    };
    let strengths = |sensor| {
        let mut b = mapped(vec![mapping(sensor)]);
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
        canvas.active_layer_idx = 1;
        let (mut undo, mut tiles) = (empty_undo(), StrokeTiles::default());
        let mut stroke = StrokeState::with_seed(7);
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        for (p, t) in line(64.0, 0.5) {
            stroke.add_sample(&mut b, p, 1.0, Some(t), &mut ctx);
        }
        stroke.finish(&mut b, &mut ctx);
        stroke
            .painted
            .iter()
            .map(|v| v.strength)
            .collect::<Vec<f32>>()
    };
    let per_stroke = strengths(Sensor::RandomStroke);
    assert!(per_stroke.windows(2).all(|w| w[0] == w[1]), "one value");
    let per_dab = strengths(Sensor::RandomDab);
    assert!(per_dab.windows(2).any(|w| w[0] != w[1]), "each dab its own");
}

#[test]
fn an_angle_mapping_turns_the_tip_and_hue_shifts_the_colour() {
    use crate::brush_engine::dynamics::{DabSetting, DabVar, InputMapping, Sensor, SensorValues};
    let s = SensorValues {
        pressure: 1.0,
        ..Default::default()
    };
    let mut v = DabVar::default();
    for (setting, amount) in [
        (DabSetting::Angle, 0.5),
        (DabSetting::Hue, -0.5),
        (DabSetting::Squash, 0.5),
    ] {
        InputMapping {
            sensor: Sensor::Pressure,
            setting,
            amount,
            ..Default::default()
        }
        .apply(&mut v, &s);
    }
    assert!((v.turn - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    assert_eq!(v.hsv[0], -90.0);
    assert_eq!(v.squash, 0.5);
}

#[test]
fn mappings_survive_a_preset_file() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let b = mapped(vec![InputMapping {
        sensor: Sensor::Speed,
        setting: DabSetting::Value,
        amount: -0.3,
        ..Default::default()
    }]);
    let preset = crate::brush_engine::brush::BrushPreset {
        name: "mapped".into(),
        brush: b.clone(),
        file: None,
    };
    let bytes = crate::brush_engine::preset_file::encode(&[preset]).unwrap();
    let back = crate::brush_engine::preset_file::decode(&bytes).unwrap();
    assert_eq!(back[0].brush.inputs, b.inputs);
}
