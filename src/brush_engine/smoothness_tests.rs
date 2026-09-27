//! Values that must change smoothly from dab to dab,
//! whatever the input's sample rate: tilt, stroke direction, pressure on
//! big brushes, and the stabiliser.

use crate::brush_engine::brush::{Brush, StabilizerAlgorithm};
use crate::brush_engine::dynamics::{BrushDynamics, PenTilt, TipShape};
use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use crate::canvas::Canvas;
use crate::canvas::history::UndoAction;
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;

/// One input sample: position, pressure, time, tilt.
type Sample = (Vec2, f32, f64, Option<PenTilt>);

fn run(brush: &mut Brush, samples: &[Sample], size: (usize, usize)) -> (Canvas, StrokeState) {
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let mut canvas = Canvas::new(size.0, size.1, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    };
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(1);
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        for &(p, pressure, t, tilt) in samples {
            stroke.tilt = tilt;
            stroke.add_sample(brush, p, pressure, Some(t), &mut ctx);
        }
        stroke.finish(brush, &mut ctx);
    }
    (canvas, stroke)
}

/// The tip angle a dab's orientation turns it by (degrees).
fn angle_of(orient: [f32; 4]) -> f32 {
    (-orient[1]).atan2(orient[0]).to_degrees()
}

/// Largest angle change between consecutive dabs (degrees).
fn max_turn(stroke: &StrokeState) -> f32 {
    stroke
        .painted
        .windows(2)
        .map(|w| {
            let d = angle_of(w[1].orient) - angle_of(w[0].orient);
            ((d + 180.0).rem_euclid(360.0) - 180.0).abs()
        })
        .fold(0.0, f32::max)
}

fn nib(tip: TipShape) -> Brush {
    let mut b = Brush::new(30.0, 100.0, Color32::BLACK, 5.0);
    b.brush_options.pressure_size = false;
    b.dynamics = BrushDynamics {
        tip,
        ..Default::default()
    };
    b
}

#[test]
fn a_tilting_pen_turns_the_tip_smoothly() {
    // The pen turns its lean from 0° to 90° over a stroke sampled like a
    // pen (every 12 px); the nib following it should turn a little per dab.
    let samples: Vec<Sample> = (0..=20)
        .map(|i| {
            let t = i as f32 / 20.0;
            let tilt = PenTilt {
                lean: 0.6,
                direction: t * std::f32::consts::FRAC_PI_2,
            };
            (
                Vec2::new(20.0 + i as f32 * 12.0, 64.0),
                1.0,
                t as f64 * 0.2,
                Some(tilt),
            )
        })
        .collect();
    let mut b = nib(TipShape {
        ratio: 0.2,
        follow_tilt: true,
        ..Default::default()
    });
    let (_, stroke) = run(&mut b, &samples, (300, 128));
    let dabs = stroke.painted.len();
    let turn = max_turn(&stroke);
    eprintln!("tilt: {dabs} dabs, max turn between dabs {turn:.2}°");
    assert!(turn < 1.5, "tilt steps: {turn:.2}° between dabs");
}

#[test]
fn the_direction_is_steady_on_a_slow_mouse_line() {
    // A slow mouse on a 20° line: integer positions about 1.5 px apart.
    let dir = 20f32.to_radians();
    let samples: Vec<Sample> = (0..160)
        .map(|i| {
            let d = i as f32 * 1.5;
            let p = Vec2::new(
                (20.0 + d * dir.cos()).round(),
                (100.0 - d * dir.sin()).round(),
            );
            (p, 1.0, i as f64 * 0.004, None)
        })
        .collect();
    let mut b = nib(TipShape {
        ratio: 0.2,
        follow_stroke: true,
        ..Default::default()
    });
    let (_, stroke) = run(&mut b, &samples, (300, 128));
    // Past the start (the first segment's whole-pixel direction takes a
    // smoothing distance to settle).
    let angles: Vec<f32> = stroke
        .painted
        .iter()
        .skip(40)
        .map(|v| angle_of(v.orient))
        .collect();
    let (lo, hi) = angles
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &a| (lo.min(a), hi.max(a)));
    let turn = angles
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    eprintln!("direction: angles {lo:.1}°..{hi:.1}° (true 20°), max turn {turn:.2}°");
    assert!(hi - lo < 6.0, "direction wobbles {lo:.1}..{hi:.1}");
    // Dabs 1.5 px apart: 3° moves a 30 px nib's edge under a pixel.
    assert!(turn < 3.0, "direction steps {turn:.2}°");
}

#[test]
fn a_big_brush_grows_with_pressure_without_steps() {
    // A 400 px brush with pressure rising slowly: the width should grow
    // without visible jumps.
    let samples: Vec<Sample> = (0..=200)
        .map(|i| {
            let t = i as f32 / 200.0;
            (
                Vec2::new(250.0 + t * 1500.0, 300.0),
                0.3 + 0.7 * t,
                t as f64,
                None,
            )
        })
        .collect();
    let mut b = Brush::new(400.0, 100.0, Color32::BLACK, 5.0);
    let (canvas, _) = run(&mut b, &samples, (2000, 600));
    let width = |x: usize| {
        (0..600)
            .filter(|&y| {
                canvas
                    .get_layer_tile_data(1, (x / 64) as i32, (y / 64) as i32)
                    .is_some_and(|t| t[(y % 64) * 64 + x % 64].a() > 127)
            })
            .count() as i32
    };
    let widths: Vec<i32> = (400..1700).step_by(4).map(width).collect();
    let jump = widths
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .max()
        .unwrap();
    eprintln!(
        "pressure: widths {}..{}, biggest jump between columns 4 px apart {jump}",
        widths[0],
        widths[widths.len() - 1]
    );
    assert!(jump <= 2, "size steps of {jump} px");
}

#[test]
fn the_stabiliser_doesnt_depend_on_the_sample_rate() {
    // The same stroke sampled like a pen (200 Hz) and like a mouse (800
    // Hz): the smoothing should be about the same.
    let path = |rate: usize| -> Vec<Sample> {
        (0..=rate)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (
                    Vec2::new(20.0 + t * 200.0, 64.0 + (t * 12.0).sin() * 30.0),
                    1.0,
                    t as f64,
                    None,
                )
            })
            .collect()
    };
    let lag = |rate: usize| {
        let mut b = Brush::new(6.0, 100.0, Color32::BLACK, 10.0);
        b.stabilizer_algorithm = StabilizerAlgorithm::Simple;
        b.stabilizer = 0.6;
        let (_, stroke) = run(&mut b, &path(rate), (300, 128));
        let end = stroke.last_pos.unwrap();
        (end - Vec2::new(220.0, 64.0 + 12f32.sin() * 30.0)).length()
    };
    let (pen, mouse) = (lag(200), lag(800));
    eprintln!(
        "stabiliser: lag behind the pen at the end {pen:.1} px (200 Hz) vs {mouse:.1} px (800 Hz)"
    );
    assert!(
        (pen - mouse).abs() < pen.max(mouse) * 0.35 + 0.5,
        "{pen:.1} vs {mouse:.1}"
    );
}
