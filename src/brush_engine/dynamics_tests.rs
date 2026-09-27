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
