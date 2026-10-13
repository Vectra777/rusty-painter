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

#[test]
fn a_thin_nib_dragged_edge_on_leaves_no_gaps() {
    // A nib 40 px wide and 4 thick, spaced a quarter of its width (10 px),
    // drawn straight down: spaced by its width the dabs would stand apart.
    let mut b = Brush::new(40.0, 100.0, Color32::BLACK, 25.0);
    b.brush_options.pressure_size = false;
    b.dynamics.tip = TipShape {
        ratio: 0.1,
        ..TipShape::default()
    };
    let samples: Vec<Sample> = (0..=27)
        .map(|i| {
            (
                Vec2::new(100.0, 10.0 + i as f32 * 4.0),
                1.0,
                i as f64 * 0.01,
                None,
            )
        })
        .collect();
    let (canvas, _) = run(&mut b, &samples, (200, 128));
    let alpha = |y: usize| {
        canvas
            .get_layer_tile_data(1, 100 / 64, (y / 64) as i32)
            .map_or(0, |t| t[(y % 64) * 64 + 100 % 64].a())
    };
    let gaps: Vec<usize> = (14..114).filter(|&y| alpha(y) < 128).collect();
    assert!(gaps.is_empty(), "unpainted rows {gaps:?}");
}

#[test]
fn a_fast_curve_stays_round_between_far_apart_samples() {
    // A fast circle as a pen reports it at 240 Hz: samples 100 px apart.
    // Straight pieces between them would cut inside it by 6 px.
    let (c, r) = (Vec2::new(300.0, 300.0), 200.0);
    let step = 100.0 / r;
    let samples: Vec<Sample> = (0..=10)
        .map(|i| {
            let a = i as f32 * step;
            (
                c + Vec2::new(a.cos(), a.sin()) * r,
                1.0,
                i as f64 / 240.0,
                None,
            )
        })
        .collect();
    let mut b = Brush::new(4.0, 100.0, Color32::BLACK, 5.0);
    b.brush_options.pressure_size = false;
    let (canvas, _) = run(&mut b, &samples, (600, 600));
    // How far painted pixels stray from the circle, past the first two
    // pieces (no direction is known before the first, and the second leaves
    // the way it went).
    let mut worst = 0f32;
    for y in 0..600 {
        for x in 0..600 {
            let painted = canvas
                .get_layer_tile_data(1, (x / 64) as i32, (y / 64) as i32)
                .is_some_and(|t| t[(y % 64) * 64 + x % 64].a() > 127);
            let d = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - c;
            if painted && d.y.atan2(d.x) > step * 2.0 {
                worst = worst.max((d.length() - r).abs());
            }
        }
    }
    eprintln!("fast curve: painted pixels up to {worst:.2} px off the circle");
    assert!(worst < 3.0, "{worst:.2} px off the circle");
}

#[test]
fn a_big_widely_spaced_brush_widens_without_notches() {
    // A 233 px brush at 16 % spacing (Ink-7's), pressure rising slowly:
    // its edge should rise a little at a time, not in steps.
    let samples: Vec<Sample> = (0..=300)
        .map(|i| {
            let t = i as f32 / 300.0;
            (
                Vec2::new(200.0 + t * 1400.0, 300.0),
                0.4 + 0.3 * t,
                t as f64,
                None,
            )
        })
        .collect();
    let mut b = Brush::new(233.0, 100.0, Color32::BLACK, 16.0);
    let (canvas, _) = run(&mut b, &samples, (1800, 600));
    // The top edge's height in each column.
    let top = |x: usize| {
        (0..600)
            .find(|&y| {
                canvas
                    .get_layer_tile_data(1, (x / 64) as i32, (y / 64) as i32)
                    .is_some_and(|t| t[(y % 64) * 64 + x % 64].a() > 127)
            })
            .unwrap_or(600) as i32
    };
    let edge: Vec<f32> = (400..1500).map(|x| top(x) as f32).collect();
    // Against its own trend (75 px around, two dabs' spacing): the dabs'
    // own scallops are a pixel or so; a step in width shows more.
    let wobble = (37..edge.len() - 37)
        .map(|i| {
            let avg = edge[i - 37..=i + 37].iter().sum::<f32>() / 75.0;
            (edge[i] - avg).abs()
        })
        .fold(0.0, f32::max);
    eprintln!("big brush: its edge strays {wobble:.2} px from its trend");
    assert!(wobble < 1.1, "the edge wobbles {wobble:.2} px");
}

#[test]
fn pressure_opacity_fades_without_bands() {
    // A small brush whose opacity follows pressure, pressed slowly harder:
    // its transparency should change a level or two at a time (in 64
    // pressure steps it rose in stairs of up to 5).
    let samples: Vec<Sample> = (0..=300)
        .map(|i| {
            let t = i as f32 / 300.0;
            (
                Vec2::new(100.0 + t * 1400.0, 50.0),
                0.2 + 0.6 * t,
                t as f64,
                None,
            )
        })
        .collect();
    let mut b = Brush::new(12.0, 100.0, Color32::BLACK, 5.0);
    b.brush_options.pressure_size = false;
    b.brush_options.pressure_opacity = true;
    // Wash: pressure sets the opacity the stroke settles at (build-up
    // would pile the dabs up to opaque).
    b.brush_options.painting_mode = crate::brush_engine::brush_options::PaintingMode::Wash;
    let (canvas, _) = run(&mut b, &samples, (1600, 100));
    let alpha = |x: usize| {
        canvas
            .get_layer_tile_data(1, (x / 64) as i32, 0)
            .map_or(0, |t| t[50 * 64 + x % 64].a() as i32)
    };
    let row: Vec<i32> = (300..1400).map(alpha).collect();
    let step = row.windows(2).map(|w| (w[1] - w[0]).abs()).max().unwrap();
    eprintln!("opacity: biggest step between columns {step}/255");
    assert!(step <= 2, "opacity steps {step}/255 at once");
}

#[test]
fn smoothing_eases_pressure_with_the_position() {
    // Strong smoothing; the pen presses hard all at once half way. The
    // brush is held back from the pen, and its pressure with it: the width
    // grows over the stretch the smoothing eases over, not in one step.
    let samples: Vec<Sample> = (0..=120)
        .map(|i| {
            let p = if i < 60 { 0.3 } else { 1.0 };
            (
                Vec2::new(100.0 + i as f32 * 10.0, 200.0),
                p,
                i as f64 / 120.0,
                None,
            )
        })
        .collect();
    let mut b = Brush::new(120.0, 100.0, Color32::BLACK, 5.0);
    b.stabilizer_algorithm = StabilizerAlgorithm::Simple;
    b.stabilizer = 0.9;
    let (canvas, _) = run(&mut b, &samples, (1400, 400));
    let top = |x: usize| {
        (0..400)
            .find(|&y| {
                canvas
                    .get_layer_tile_data(1, (x / 64) as i32, (y / 64) as i32)
                    .is_some_and(|t| t[(y % 64) * 64 + x % 64].a() > 127)
            })
            .unwrap_or(400) as i32
    };
    let edge: Vec<i32> = (300..1200).map(top).collect();
    let step = edge.windows(10).map(|w| (w[9] - w[0]).abs()).max().unwrap();
    eprintln!("smoothed: the edge moves at most {step} px over 10 columns");
    assert!(step <= 10, "the width jumps {step} px over 10 columns");
}

#[test]
fn a_light_stroke_sized_by_an_input_stays_unbroken() {
    // Size by pressure through Inputs, pressed lightly: the dabs are small,
    // and spaced by their size, not the brush's (which left gaps).
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let samples: Vec<Sample> = (0..=100)
        .map(|i| {
            (
                Vec2::new(100.0 + i as f32 * 8.0, 100.0),
                0.08,
                i as f64 / 240.0,
                None,
            )
        })
        .collect();
    let mut b = Brush::new(60.0, 100.0, Color32::BLACK, 25.0);
    b.brush_options.pressure_size = false;
    b.inputs = vec![InputMapping {
        sensor: Sensor::Pressure,
        setting: DabSetting::Size,
        amount: 1.0,
        ..Default::default()
    }];
    let (canvas, _) = run(&mut b, &samples, (1000, 200));
    let alpha = |x: usize| {
        canvas
            .get_layer_tile_data(1, (x / 64) as i32, 1)
            .map_or(0, |t| t[(100 % 64) * 64 + x % 64].a())
    };
    let gaps = (150..850).filter(|&x| alpha(x) < 40).count();
    eprintln!("light input-sized stroke: {gaps} columns with a gap");
    assert_eq!(gaps, 0, "{gaps} columns between dabs left empty");
}

#[test]
fn a_steep_pressure_curve_makes_no_steps_either() {
    // Nearly all of the size in a tenth of the pressure: rounding pressure
    // (as it was) gave steps of several pixels; dabs are grouped by how big
    // they are now.
    use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
    let samples: Vec<Sample> = (0..=300)
        .map(|i| {
            let t = i as f32 / 300.0;
            (
                Vec2::new(200.0 + t * 1400.0, 300.0),
                0.5 + 0.1 * t,
                t as f64,
                None,
            )
        })
        .collect();
    let mut b = Brush::new(233.0, 100.0, Color32::BLACK, 16.0);
    let point = |x, y| CurvePoint { x, y };
    b.brush_options.pressure_curves.size = Some(SoftnessCurve {
        points: vec![
            point(0.0, 0.0),
            point(0.5, 0.3),
            point(0.6, 1.0),
            point(1.0, 1.0),
        ],
    });
    let (canvas, _) = run(&mut b, &samples, (1800, 600));
    let top = |x: usize| {
        (0..600)
            .find(|&y| {
                canvas
                    .get_layer_tile_data(1, (x / 64) as i32, (y / 64) as i32)
                    .is_some_and(|t| t[(y % 64) * 64 + x % 64].a() > 127)
            })
            .unwrap_or(600) as f32
    };
    let edge: Vec<f32> = (400..1500).map(top).collect();
    let wobble = (37..edge.len() - 37)
        .map(|i| (edge[i] - edge[i - 37..=i + 37].iter().sum::<f32>() / 75.0).abs())
        .fold(0.0, f32::max);
    eprintln!("steep curve: the edge strays {wobble:.2} px from its trend");
    assert!(wobble < 1.5, "the edge wobbles {wobble:.2} px");
}

#[test]
fn pressing_harder_in_place_grows_the_mark() {
    let samples: Vec<Sample> = (0..=30)
        .map(|i| {
            (
                Vec2::new(150.0, 150.0),
                0.1 + 0.03 * i as f32,
                i as f64 / 60.0,
                None,
            )
        })
        .collect();
    let mut b = Brush::new(200.0, 100.0, Color32::BLACK, 10.0);
    let (canvas, _) = run(&mut b, &samples, (300, 300));
    let inked = (0..300)
        .filter(|&x| {
            canvas
                .get_layer_tile_data(1, x / 64, 150 / 64)
                .is_some_and(|t| t[(150 % 64) * 64 + (x % 64) as usize].a() > 127)
        })
        .count();
    eprintln!("pressed in place: {inked} px across");
    assert!(inked > 150, "the mark stayed {inked} px across");
}

#[test]
fn pressure_settings_folded_into_inputs_paint_the_same() {
    // The Pen pressure settings and the Pressure inputs they become: the
    // same stroke either way.
    use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
    let samples: Vec<Sample> = (0..=200)
        .map(|i| {
            let t = i as f32 / 200.0;
            let p = 0.1 + 0.9 * (t * std::f32::consts::PI).sin();
            (
                Vec2::new(100.0 + t * 1000.0, 200.0 + 60.0 * (t * 9.0).sin()),
                p,
                t as f64,
                None,
            )
        })
        .collect();
    let mut a = Brush::new(80.0, 60.0, Color32::BLACK, 12.0);
    let o = &mut a.brush_options;
    (o.pressure_size, o.pressure_min_size, o.pressure_opacity) = (true, 0.2, true);
    o.pressure_curves.size = Some(SoftnessCurve {
        points: vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.4, y: 0.7 },
            CurvePoint { x: 1.0, y: 1.0 },
        ],
    });
    let mut b = a.clone();
    b.fold_pressure();
    assert!(
        !b.brush_options.pressure_size && b.inputs.len() == 2,
        "folded"
    );
    let mut again = b.clone();
    again.fold_pressure();
    assert_eq!(again.inputs, b.inputs, "folding twice changes nothing");
    let (ca, _) = run(&mut a, &samples, (1200, 400));
    let (cb, _) = run(&mut b, &samples, (1200, 400));
    let mut worst = 0u8;
    for ty in 0..(400 / 64 + 1) {
        for tx in 0..(1200 / 64 + 1) {
            let (pa, pb) = (
                ca.get_layer_tile_data(1, tx, ty),
                cb.get_layer_tile_data(1, tx, ty),
            );
            let (pa, pb) = (pa.unwrap_or_default(), pb.unwrap_or_default());
            for (x, y) in pa.iter().zip(pb.iter()) {
                worst = worst.max(x.a().abs_diff(y.a()));
            }
        }
    }
    eprintln!("folded vs settings: alphas differ by at most {worst}");
    assert!(worst <= 1, "alphas differ by {worst}");
}
