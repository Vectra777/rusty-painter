//! Inputs mapped to settings: distance, random per stroke, pressure, angle, hardness, texture, scatter, colour mix, grain placement and flips.

use super::*;

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
fn pressure_in_holds_the_highest_pressure_so_far() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let strengths = |sensor| {
        let mut b = mapped(vec![InputMapping {
            sensor,
            setting: DabSetting::Opacity,
            amount: 1.0,
            ..Default::default()
        }]);
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
        canvas.active_layer_idx = 1;
        let (mut undo, mut tiles) = (empty_undo(), StrokeTiles::default());
        let mut stroke = StrokeState::with_seed(7);
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        // A press that peaks halfway, then eases off.
        let samples = line(64.0, 0.5);
        let n = samples.len() as f32;
        for (i, (p, t)) in samples.into_iter().enumerate() {
            let pressure = 1.0 - (2.0 * i as f32 / n - 1.0).abs();
            stroke.add_sample(&mut b, p, pressure, Some(t), &mut ctx);
        }
        stroke.finish(&mut b, &mut ctx);
        stroke
            .painted
            .iter()
            .map(|v| v.strength)
            .collect::<Vec<f32>>()
    };
    let held = strengths(Sensor::PressureIn);
    assert!(
        held.windows(2).all(|w| w[1] >= w[0] - 1e-6),
        "never eases off"
    );
    let plain = strengths(Sensor::Pressure);
    assert!(plain.last() < held.last(), "pressure itself does");
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

#[test]
fn an_input_mapped_to_hardness_changes_the_edge() {
    use crate::brush_engine::dynamics::DabSetting;
    let soft = || {
        let mut b = Brush::new(30.0, 0.0, Color32::BLACK, 5.0);
        b.brush_options.pressure_size = false;
        b
    };
    let (plain, _) = paint(&mut soft(), &line(64.0, 0.5), 1, true);
    let mut hard = soft();
    hard.inputs = vec![map_pressure(DabSetting::Hardness, 1.0)];
    let (hardened, _) = paint(&mut hard, &line(64.0, 0.5), 1, true);
    assert!(
        soft_edge(&hardened, 128) * 3 < soft_edge(&plain, 128),
        "crisper: {} vs {}",
        soft_edge(&hardened, 128),
        soft_edge(&plain, 128)
    );
    // The other way softens a hard brush.
    let mut softened = Brush::new(30.0, 100.0, Color32::BLACK, 5.0);
    softened.brush_options.pressure_size = false;
    let (crisp, _) = paint(&mut softened.clone(), &line(64.0, 0.5), 1, true);
    softened.inputs = vec![map_pressure(DabSetting::Hardness, -1.0)];
    let (blurry, _) = paint(&mut softened, &line(64.0, 0.5), 1, true);
    assert!(soft_edge(&blurry, 128) > soft_edge(&crisp, 128) * 3);
}

#[test]
fn an_input_mapped_to_texture_strength_scales_the_grain() {
    use crate::brush_engine::dynamics::DabSetting;
    let (grainy, _) = paint(&mut textured(), &line(64.0, 0.5), 1, true);
    let mut none = textured();
    none.texture = None;
    let (smooth, _) = paint(&mut none, &line(64.0, 0.5), 1, true);
    // Full pressure, amount -1: no grain at all.
    let mut off = textured();
    off.inputs = vec![map_pressure(DabSetting::TextureStrength, -1.0)];
    let (mapped, _) = paint(&mut off, &line(64.0, 0.5), 1, true);
    assert!(pixels(&mapped) == pixels(&smooth), "no grain left");
    assert!(pixels(&grainy) != pixels(&smooth));
    // Halfway: in between.
    let mut half = textured();
    half.inputs = vec![map_pressure(DabSetting::TextureStrength, -0.5)];
    let (halfway, _) = paint(&mut half, &line(64.0, 0.5), 1, true);
    let sum = |c: &Canvas| pixels(c).iter().map(|&a| a as u32).sum::<u32>();
    assert!(sum(&grainy) < sum(&halfway) && sum(&halfway) < sum(&smooth));
}

#[test]
fn an_input_mapped_to_scatter_scatters_repeatably() {
    use crate::brush_engine::dynamics::DabSetting;
    let mut b = brush(BrushDynamics::default());
    b.inputs = vec![map_pressure(DabSetting::Scatter, 1.0)];
    let (a, _) = paint(&mut b.clone(), &line(64.0, 0.5), 3, true);
    let (again, _) = paint(&mut b.clone(), &line(64.0, 0.5), 3, true);
    assert!(pixels(&a) == pixels(&again), "the same seed, the same dabs");
    let (other, _) = paint(&mut b.clone(), &line(64.0, 0.5), 4, true);
    assert!(pixels(&a) != pixels(&other));
    // Up to a brush width each way: paint well off the line.
    let far = |c: &Canvas| {
        (0..W)
            .flat_map(|x| (0..H).map(move |y| (x, y)))
            .filter(|&(x, y)| (y as i32 - 64).abs() > 14 && alpha(c, x, y) > 0)
            .count()
    };
    assert!(far(&a) > 50, "{}", far(&a));
    let (plain, _) = paint(
        &mut brush(BrushDynamics::default()),
        &line(64.0, 0.5),
        3,
        true,
    );
    assert_eq!(far(&plain), 0);
}

#[test]
fn an_input_mapped_to_colour_mix_paints_the_secondary_colour() {
    use crate::brush_engine::dynamics::DabSetting;
    let colour = |amount: f32| {
        let mut b = brush(BrushDynamics::default());
        b.second_color = Color32::from_rgb(220, 20, 20);
        b.inputs = vec![map_pressure(DabSetting::ColorMix, amount)];
        let (c, _) = paint(&mut b, &line(64.0, 0.5), 1, true);
        pixel(&c, 128, 64)
    };
    let full = colour(1.0);
    assert!(full.r() > 200 && full.g() < 40, "red: {full:?}");
    let half = colour(0.5);
    assert!(
        (95..135).contains(&half.r()) && half.g() < 30,
        "half way to red: {half:?}"
    );
    assert_eq!(colour(0.0), Color32::BLACK);
}

#[test]
fn new_input_settings_survive_a_preset_file() {
    use crate::brush_engine::dynamics::DabSetting;
    let mut b = textured();
    b.inputs = [
        DabSetting::TextureStrength,
        DabSetting::Hardness,
        DabSetting::Scatter,
        DabSetting::ColorMix,
    ]
    .into_iter()
    .map(|s| map_pressure(s, 0.4))
    .collect();
    if let Some(t) = b.texture.as_mut() {
        t.placement = crate::brush_engine::texture::GrainPlacement {
            follow_stroke: true,
            angle: 30.0,
            random_offset: true,
            per_dab: true,
        };
    }
    let preset = crate::brush_engine::brush::BrushPreset {
        name: "new inputs".into(),
        brush: b.clone(),
        file: None,
    };
    let bytes = crate::brush_engine::preset_file::encode(&[preset]).unwrap();
    let back = crate::brush_engine::preset_file::decode(&bytes).unwrap();
    assert_eq!(back[0].brush.inputs, b.inputs);
    assert_eq!(back[0].brush.texture, b.texture);
}

#[test]
fn grain_can_move_with_the_stroke() {
    use crate::brush_engine::texture::GrainPlacement;
    let moving = GrainPlacement {
        follow_stroke: true,
        ..Default::default()
    };
    // Started 37 px further along: the same stroke, grain and all.
    let (a, b) = (
        placed_stroke(moving, 20.0, 1),
        placed_stroke(moving, 57.0, 1),
    );
    assert_eq!(unlike(&window(&a, 10, 100), &window(&b, 47, 100)), 0);
    // Pinned to the canvas, the grain stays put instead.
    let pinned = GrainPlacement::default();
    let (a, b) = (
        placed_stroke(pinned, 20.0, 1),
        placed_stroke(pinned, 57.0, 1),
    );
    assert!(unlike(&window(&a, 10, 100), &window(&b, 47, 100)) > 200);
}

#[test]
fn a_random_offset_differs_each_stroke_but_repeats_with_a_seed() {
    use crate::brush_engine::texture::GrainPlacement;
    let random = GrainPlacement {
        random_offset: true,
        ..Default::default()
    };
    let one = pixels(&placed_stroke(random, 20.0, 1));
    assert!(one == pixels(&placed_stroke(random, 20.0, 1)), "repeatable");
    assert!(
        one != pixels(&placed_stroke(random, 20.0, 2)),
        "each stroke its own"
    );
    // Without it, strokes share the grain whatever the seed.
    let plain = GrainPlacement::default();
    assert!(pixels(&placed_stroke(plain, 20.0, 1)) == pixels(&placed_stroke(plain, 20.0, 2)));
}

#[test]
fn texture_each_dab_gives_every_dab_the_same_grain() {
    use crate::brush_engine::texture::GrainPlacement;
    // Two dabs whole pixels apart look the same, grain and all.
    let dab = |placement, at: Vec2| {
        let mut b = textured();
        if let Some(t) = b.texture.as_mut() {
            t.placement = placement;
        }
        paint(&mut b, &[(at, 0.0)], 1, true).0
    };
    let each = GrainPlacement {
        per_dab: true,
        ..Default::default()
    };
    let (a, b) = (
        dab(each, Vec2::new(60.25, 64.0)),
        dab(each, Vec2::new(141.25, 64.0)),
    );
    assert_eq!(unlike(&window(&a, 40, 40), &window(&b, 121, 40)), 0);
    let pinned = GrainPlacement::default();
    let (a, b) = (
        dab(pinned, Vec2::new(60.25, 64.0)),
        dab(pinned, Vec2::new(141.25, 64.0)),
    );
    assert!(unlike(&window(&a, 40, 40), &window(&b, 121, 40)) > 50);
}

#[test]
fn placed_grain_and_new_inputs_undo_exactly() {
    use crate::brush_engine::texture::GrainPlacement;
    let mut b = textured();
    if let Some(t) = b.texture.as_mut() {
        t.placement = GrainPlacement {
            follow_stroke: true,
            angle: 40.0,
            random_offset: true,
            per_dab: false,
        };
    }
    b.inputs = vec![map_pressure(
        crate::brush_engine::dynamics::DabSetting::Hardness,
        0.5,
    )];
    let (mut canvas, undo) = paint(&mut b, &line(64.0, 0.5), 1, true);
    let blank = pixels(&Canvas::new(W, H, Color32::WHITE, 64));
    assert!(pixels(&canvas) != blank);
    let mut history = History::new();
    history.push_action(undo);
    let mut selection = crate::selection::SelectionManager::new();
    let mut tool = crate::app::tools::Tool::Brush;
    history.undo(&mut canvas, &mut selection, &mut tool);
    assert!(pixels(&canvas) == blank);
}

#[test]
fn random_flips_mirror_some_dabs_and_not_others() {
    // Dabs 20 px apart along y = 64: each paints the left of its centre,
    // or, mirrored, the right.
    let sides = |flip: bool| {
        let mut b = brush(BrushDynamics {
            tip: TipShape {
                random_flip_x: flip,
                ..Default::default()
            },
            ..Default::default()
        });
        b.brush_options.diameter = 10.0;
        b.brush_options.spacing = 200.0;
        b.brush_options.pixel_shape = left_half_tip();
        let (canvas, _) = paint(&mut b, &line(64.0, 0.5), 7, true);
        let (mut left, mut right) = (0, 0);
        for cx in (20..=220).step_by(20) {
            let (l, r) = (alpha(&canvas, cx - 3, 64), alpha(&canvas, cx + 3, 64));
            assert!(l.abs_diff(r) > 100, "one side of the dab at {cx}: {l} {r}");
            if l > r {
                left += 1;
            } else {
                right += 1;
            }
        }
        (left, right)
    };
    assert_eq!(sides(false), (11, 0), "unflipped: all on the left");
    let (left, right) = sides(true);
    assert!(
        left >= 2 && right >= 2,
        "some each way: {left} left, {right} right"
    );
}

#[test]
fn pressure_spacing_closes_the_gaps_under_light_pressure() {
    let gaps = |on: bool| {
        let mut b = brush(BrushDynamics::default());
        b.brush_options.diameter = 8.0;
        b.brush_options.spacing = 200.0;
        b.brush_options.pressure_spacing = on;
        let canvas = paint_pen(&mut b, &line(64.0, 0.5), 0.2, None);
        (40..200).filter(|&x| alpha(&canvas, x, 64) < 64).count()
    };
    let (spaced, closed) = (gaps(false), gaps(true));
    assert!(spaced > 60, "200% spacing leaves gaps: {spaced}");
    assert_eq!(closed, 0, "at a fifth of the spacing the dabs meet");
}

#[test]
fn sharpness_makes_a_soft_dab_hard_edged() {
    let alphas = |sharpness: f32| {
        let mut b = brush(BrushDynamics::default());
        b.brush_options.diameter = 40.0;
        b.brush_options.hardness = 0.0;
        b.sharpness = sharpness;
        let (canvas, _) = paint(&mut b, &[(Vec2::new(128.0, 64.0), 0.0)], 1, true);
        pixels(&canvas)
    };
    let soft = alphas(0.0);
    assert!(soft.iter().any(|&a| a > 10 && a < 245), "soft edges");
    let hard = alphas(0.5);
    assert!(hard.iter().all(|&a| a == 0 || a == 255), "all or nothing");
    let (s, h) = (
        soft.iter().filter(|&&a| a > 0).count(),
        hard.iter().filter(|&&a| a > 0).count(),
    );
    assert!(h > 50 && h < s, "the faint rim is cut: {h} of {s}");
}

#[test]
fn a_nib_that_follows_the_barrel_turns_with_it() {
    use crate::brush_engine::dynamics::PenBarrel;
    let mut b = brush(BrushDynamics {
        tip: TipShape {
            ratio: 0.2,
            follow_barrel: true,
            ..Default::default()
        },
        ..Default::default()
    });
    b.brush_options.diameter = 40.0;
    let extent = |rotation: Option<f32>| {
        let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
        canvas.active_layer_idx = 1;
        let mut undo = empty_undo();
        let mut tiles = StrokeTiles::default();
        let mut stroke = StrokeState::with_seed(1);
        {
            let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
            stroke.barrel = PenBarrel {
                rotation,
                wheel: None,
            };
            let mut b = b.clone();
            stroke.add_sample(&mut b, Vec2::new(128.0, 64.0), 1.0, Some(0.0), &mut ctx);
            stroke.finish(&mut b, &mut ctx);
        }
        let painted: Vec<(usize, usize)> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, y)| alpha(&canvas, x, y) > 127)
            .collect();
        let span = |f: fn(&(usize, usize)) -> usize| {
            painted.iter().map(f).max().unwrap() - painted.iter().map(f).min().unwrap()
        };
        (span(|p| p.0), span(|p| p.1))
    };
    let (w, h) = extent(Some(0.0));
    assert!(w > h * 3, "not turned: long across, {w}×{h}");
    let (w, h) = extent(Some(std::f32::consts::FRAC_PI_2));
    assert!(h > w * 3, "turned a quarter: long up and down, {w}×{h}");
    assert_eq!(
        extent(None),
        extent(Some(0.0)),
        "no rotation reported: upright"
    );
}

#[test]
fn the_new_stroke_features_paint_and_undo_exactly() {
    use crate::brush_engine::texture::{BrushTexture, TextureMode, builtin};
    let below = Color32::from_rgb(235, 230, 220);
    type Setup = Box<dyn Fn(&mut Brush)>;
    let features: Vec<(&str, Setup)> = vec![
        (
            "flips",
            Box::new(|b: &mut Brush| {
                b.brush_options.pixel_shape = left_half_tip();
                b.dynamics.tip.random_flip_x = true;
                b.dynamics.tip.random_flip_y = true;
            }),
        ),
        (
            "hard edges",
            Box::new(|b: &mut Brush| {
                b.brush_options.hardness = 0.0;
                b.sharpness = 0.5;
            }),
        ),
        (
            "pressure spacing",
            Box::new(|b: &mut Brush| {
                b.brush_options.pressure_spacing = true;
            }),
        ),
        (
            "colour dodge",
            Box::new(|b: &mut Brush| {
                let mut t = BrushTexture::new(builtin()[0].clone());
                t.mode = TextureMode::ColorDodge;
                b.texture = Some(t);
            }),
        ),
        (
            "hard mix",
            Box::new(|b: &mut Brush| {
                let mut t = BrushTexture::new(builtin()[0].clone());
                t.mode = TextureMode::HardMix;
                b.texture = Some(t);
            }),
        ),
        (
            "parallel",
            Box::new(|b: &mut Brush| {
                b.paint_blend = crate::canvas::blend_modes::LayerBlend::Parallel;
            }),
        ),
    ];
    for (name, set) in features {
        let mut brush = Brush::new(24.0, 60.0, Color32::from_rgb(40, 90, 160), 10.0);
        set(&mut brush);
        let before = pixels_rgba(&painted(below));
        let (mut canvas, undo) = paint_preset(&mut brush, below);
        let changed = before
            .iter()
            .zip(&pixels_rgba(&canvas))
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 500, "{name}: only {changed} pixels changed");
        let mut history = History::new();
        history.push_action(undo);
        let mut selection = crate::selection::SelectionManager::new();
        let mut tool = crate::app::tools::Tool::Brush;
        history.undo(&mut canvas, &mut selection, &mut tool);
        assert!(pixels_rgba(&canvas) == before, "{name}: undo isn't exact");
    }
}

#[test]
fn the_new_features_are_repeatable_with_a_seed() {
    let mut b = Brush::new(24.0, 60.0, Color32::BLACK, 10.0);
    b.brush_options.pixel_shape = left_half_tip();
    b.dynamics.tip.random_flip_x = true;
    b.sharpness = 0.3;
    let a = pixels(&paint(&mut b.clone(), &line(64.0, 0.5), 5, true).0);
    let again = pixels(&paint(&mut b.clone(), &line(64.0, 0.5), 5, true).0);
    assert!(a == again, "the same seed paints the same stroke");
}
