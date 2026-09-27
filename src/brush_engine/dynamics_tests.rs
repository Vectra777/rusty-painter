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
