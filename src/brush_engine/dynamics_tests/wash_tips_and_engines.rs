//! Washes, hard edges by pressure, lightness tips, tip shaping, and the Krita-style engines (spray, chalk, grid, tangent normal, curve, particle).

use super::*;

#[test]
fn wash_never_goes_past_the_pressure_s_opacity_however_often_it_s_gone_over() {
    // Alpha darken: at light pressure, scrubbing stays light.
    for (passes, pressure) in [(1, 0.3), (8, 0.3), (8, 0.7)] {
        let canvas = scrub(&mut wash_brush(100.0), passes, pressure);
        let a = alpha(&canvas, 120, 64) as f32 / 255.0;
        assert!(
            (a - pressure).abs() < 0.02,
            "{passes} passes at {pressure}: {a}"
        );
    }
}

#[test]
fn wash_flow_approaches_the_opacity_gradually() {
    let once = alpha(&scrub(&mut wash_brush(20.0), 1, 0.6), 120, 64) as f32 / 255.0;
    let many = alpha(&scrub(&mut wash_brush(20.0), 8, 0.6), 120, 64) as f32 / 255.0;
    assert!(once < many, "builds with each pass: {once} then {many}");
    assert!(many <= 0.61, "never past the opacity: {many}");
}

#[test]
fn easing_pressure_in_wash_fades_gradually_not_at_once() {
    // Full pressure, then light, along a line: the average opacity carries
    // the strong part on for a while rather than dropping at once.
    let mut b = wash_brush(100.0);
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut undo = empty_undo();
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::with_seed(1);
    {
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        for i in 0..=60 {
            let p = if i < 30 { 1.0 } else { 0.2 };
            let pos = Vec2::new(20.0 + i as f32 * 3.6, 64.0);
            stroke.add_sample(&mut b, pos, p, Some(i as f64 * 0.01), &mut ctx);
        }
        stroke.finish(&mut b, &mut ctx);
    }
    let at = |x: usize| alpha(&canvas, x, 64) as f32 / 255.0;
    assert!(at(60) > 0.95, "strong part: {}", at(60));
    assert!((at(220) - 0.2).abs() < 0.05, "light part: {}", at(220));
    assert!(at(140) > 0.3, "the change eases in: {}", at(140));
}

#[test]
fn hard_edges_follow_pressure_and_keep_a_soft_band() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let mut b = brush(BrushDynamics::default());
    b.brush_options.diameter = 40.0;
    b.brush_options.hardness = 0.0;
    b.sharpness = 0.3;
    b.inputs = vec![InputMapping {
        sensor: Sensor::Pressure,
        setting: DabSetting::Sharpness,
        ..Default::default()
    }];
    // The threshold scales with pressure, so a light dab is cut
    // nearer its middle.
    let (full, _) = dab_alphas(&mut b, 1.0);
    let (light, _) = dab_alphas(&mut b, 0.3);
    assert!(light < full * 3 / 4, "light {light} vs full {full}");
    // Without a soft band it's all or nothing; with one, some of the edge
    // keeps its own strength.
    b.inputs.clear();
    assert_eq!(dab_alphas(&mut b, 1.0).1, 0);
    b.sharpness_softness = 0.6;
    assert!(dab_alphas(&mut b, 1.0).1 > 20);
}

#[test]
fn darken_by_pressure_takes_the_colour_toward_black() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let mut b = brush(BrushDynamics::default());
    b.brush_options.color = Color32::from_rgb(200, 120, 40);
    b.inputs = vec![InputMapping {
        sensor: Sensor::Pressure,
        setting: DabSetting::Darken,
        amount: -1.0,
        ..Default::default()
    }];
    let red_at = |b: &mut Brush, p: f32| {
        let canvas = paint_pen(b, &line(64.0, 0.5), p, None);
        let tile = canvas.get_layer_tile_data(1, 2, 1).unwrap();
        tile[0].r()
    };
    let (light, hard) = (red_at(&mut b, 0.2), red_at(&mut b, 0.9));
    assert!(
        hard < light / 2,
        "darker with pressure: {light} then {hard}"
    );
}

#[test]
fn a_lightness_map_tip_paints_the_brush_colour_at_mid_grey() {
    use crate::brush_engine::brush_options::{PixelBrushShape, TipMapping};
    let grey = |g: u8| {
        PixelBrushShape::Custom(crate::brush_engine::tip::TipMask::from_colored(
            16,
            16,
            vec![255; 256],
            vec![[g; 3]; 256],
        ))
    };
    let colour = Color32::from_rgb(200, 60, 40);
    let painted = |g: u8| {
        let mut b = brush(BrushDynamics::default());
        b.brush_options.diameter = 30.0;
        b.brush_options.color = colour;
        b.brush_options.pixel_shape = grey(g);
        b.brush_options.tip_colors = true;
        b.brush_options.tip_mapping = TipMapping::Lightness;
        let (canvas, _) = paint(&mut b, &[(Vec2::new(100.0, 64.0), 0.0)], 1, true);
        // (100, 64): its middle.
        canvas.get_layer_tile_data(1, 1, 1).unwrap()[36]
    };
    let mid = painted(128);
    for (m, c) in mid.to_array().iter().zip(colour.to_array()) {
        assert!(m.abs_diff(c) <= 6, "{mid:?} vs {colour:?}");
    }
    let dark = painted(0);
    assert!(dark.r() < 10 && dark.g() < 10, "{dark:?}");
}

#[test]
fn softness_by_pressure_shrinks_a_tips_solid_core() {
    use crate::brush_engine::dynamics::DabSetting;
    use crate::brush_engine::hardness::{Softening, SoftnessSelector};
    // A hard round tip: solid to the edge until softness shrinks it.
    let hard = || {
        let mut b = brush(BrushDynamics::default());
        b.brush_options.diameter = 30.0;
        let o = &mut b.brush_options;
        o.softening = Softening::Fade(1.0);
        o.softness_selector = SoftnessSelector::Curve;
        o.softness_curve = o.softening.falloff(
            &crate::brush_engine::hardness::SoftnessCurve::default(),
            1.0,
        );
        b.inputs = vec![map_pressure(DabSetting::Softness, 1.0)];
        b
    };
    let full = paint_pen(&mut hard(), &line(64.0, 0.5), 1.0, None);
    let light = paint_pen(&mut hard(), &line(64.0, 0.5), 0.3, None);
    assert!(
        soft_edge(&light, 128) > soft_edge(&full, 128) * 2,
        "softer under light pressure: {} vs {}",
        soft_edge(&light, 128),
        soft_edge(&full, 128)
    );
    // The app's own soft tips soften too: their hardness scales down.
    let mut gaussian = Brush::new(30.0, 90.0, Color32::BLACK, 5.0);
    gaussian.brush_options.pressure_size = false;
    let crisp = paint_pen(&mut gaussian.clone(), &line(64.0, 0.5), 0.3, None);
    gaussian.inputs = vec![map_pressure(DabSetting::Softness, 1.0)];
    let blurry = paint_pen(&mut gaussian, &line(64.0, 0.5), 0.3, None);
    assert!(soft_edge(&blurry, 128) > soft_edge(&crisp, 128) * 2);
}

#[test]
fn a_mirror_input_flips_the_dabs_that_reach_half() {
    use crate::brush_engine::dynamics::DabSetting;
    // As in `random_flips_mirror_some_dabs_and_not_others`: each dab paints
    // the left of its centre, or, mirrored, the right.
    let right_side = |pressure: f32| {
        let mut b = brush(BrushDynamics {
            tip: TipShape {
                random_flip_x: true,
                ..Default::default()
            },
            ..Default::default()
        });
        b.brush_options.diameter = 10.0;
        b.brush_options.spacing = 200.0;
        b.brush_options.pixel_shape = left_half_tip();
        b.inputs = vec![map_pressure(DabSetting::Mirror, 1.0)];
        let canvas = paint_pen(&mut b, &line(64.0, 0.5), pressure, None);
        (20..=220)
            .step_by(20)
            .filter(|&cx| alpha(&canvas, cx + 3, 64) > alpha(&canvas, cx - 3, 64))
            .count()
    };
    assert_eq!(right_side(0.4), 0, "under half: none flipped");
    assert_eq!(right_side(0.6), 11, "over half: all flipped");
}

#[test]
fn fade_counts_the_strokes_dabs() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    // Size by fade over 20 dabs: the line thickens to full over its first
    // twenty dabs, then stays.
    let mut b = brush(BrushDynamics::default());
    b.brush_options.diameter = 20.0;
    b.brush_options.spacing = 25.0;
    b.inputs = vec![InputMapping {
        sensor: Sensor::Fade,
        setting: DabSetting::Size,
        length: 20.0,
        ..Default::default()
    }];
    let (canvas, _) = paint(&mut b, &line(64.0, 0.5), 1, true);
    let thick = |x: usize| (0..H).filter(|&y| alpha(&canvas, x, y) > 128).count();
    // Dabs 5 px apart from x = 20: twenty of them reach x = 120.
    assert!(
        thick(40) < thick(70) && thick(70) < thick(150),
        "{} {} {}",
        thick(40),
        thick(70),
        thick(150)
    );
    assert_eq!(thick(150), thick(200));
}

#[test]
fn spikes_make_a_squashed_tip_a_star() {
    let squashed = |spikes| {
        one_dab(|b| {
            b.dynamics.tip.ratio = 0.25;
            b.brush_options.auto_tip.spikes = spikes;
        })
    };
    let (w, h) = extent(&squashed(2));
    assert!(w >= 38 && h <= 12, "{w}×{h}");
    // Four spikes: as tall as wide, and thin between the points.
    let star = squashed(4);
    let (w, h) = extent(&star);
    assert!(w >= 38 && h >= 38, "{w}×{h}");
    assert_eq!(alpha(&star, 128 + 12, 64 + 12), 0);
}

#[test]
fn fades_across_and_down_are_their_own() {
    let canvas = one_dab(|b| b.brush_options.auto_tip.fade = [0.2, 1.0]);
    assert_eq!(alpha(&canvas, 128, 64 + 15), 255);
    assert!(alpha(&canvas, 128 + 15, 64) < 128);
}

#[test]
fn density_and_randomness_leave_a_repeatable_grain() {
    let grainy = || {
        one_dab(|b| {
            b.brush_options.auto_tip.density = 0.5;
            b.brush_options.auto_tip.randomness = 0.5;
        })
    };
    let canvas = grainy();
    let inside: Vec<u8> = (54..74)
        .flat_map(|y| (118..138).map(move |x| (x, y)))
        .map(|(x, y)| alpha(&canvas, x, y))
        .collect();
    let empty = inside.iter().filter(|&&a| a == 0).count();
    assert!((120..280).contains(&empty), "{empty} of 400 left out");
    assert!(inside.iter().any(|&a| a > 0 && a < 250), "strength varies");
    assert_eq!(pixels(&canvas), pixels(&grainy()));
}

#[test]
fn random_colour_sources_vary_by_dab_or_by_pixel() {
    use crate::brush_engine::brush_options::ColorSource;
    let colours = |source| {
        let canvas = one_dab(|b| b.brush_options.color_source = source);
        let mut seen: Vec<Color32> = (60..68)
            .flat_map(|y| (124..132).map(move |x| (x, y)))
            .map(|(x, y)| pixel(&canvas, x, y))
            .collect();
        seen.sort_by_key(|c| c.to_array());
        seen.dedup();
        seen
    };
    // One colour for the dab, not the brush's black.
    let dab = colours(ColorSource::UniformRandom);
    assert_eq!(dab.len(), 1);
    assert_ne!(dab[0], Color32::BLACK);
    assert!(colours(ColorSource::TotalRandom).len() > 30);
}

#[test]
fn flow_spacing_and_tilt_inputs_drive_their_settings() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    // No tilt reads the middle: half the flow.
    let canvas = one_dab(|b| {
        b.inputs.push(InputMapping {
            sensor: Sensor::XTilt,
            setting: DabSetting::Flow,
            ..Default::default()
        })
    });
    let a = alpha(&canvas, 128, 64);
    assert!((125..=130).contains(&a), "{a}");
    // Full pressure spacing the dabs closer builds a faint line up more.
    let line_alpha = |closer: bool| {
        let mut b = brush(BrushDynamics::default());
        b.brush_options.flow = 5.0;
        b.brush_options.spacing = 40.0;
        if closer {
            b.inputs.push(InputMapping {
                sensor: Sensor::Pressure,
                setting: DabSetting::Spacing,
                amount: -0.75,
                ..Default::default()
            });
        }
        let (canvas, _) = paint(&mut b, &line(64.0, 0.5), 1, true);
        alpha(&canvas, 128, 64)
    };
    assert!(line_alpha(true) > line_alpha(false) + 10);
}

#[test]
fn spray_paints_a_repeatable_cloud_within_its_circle() {
    use crate::brush_engine::brush::BrushType;
    let mut spray = engine(BrushType::Spray);
    let (a, _) = paint(&mut spray, &line(64.0, 1.0), 9, true);
    let (b, _) = paint(&mut spray, &line(64.0, 1.0), 9, true);
    assert!(pixels(&a) == pixels(&b), "the same stroke, the same cloud");
    let mut soft = engine(BrushType::Soft);
    let (solid, _) = paint(&mut soft, &line(64.0, 1.0), 9, true);
    assert!(
        covered(&a) > 0 && covered(&a) < covered(&solid),
        "speckled, not solid"
    );
    // Nothing past the circle (and a particle's own size).
    let far = (0..H).filter(|&y| (y as f32 - 64.0).abs() > 18.0);
    assert!(
        far.into_iter()
            .all(|y| (0..W).all(|x| alpha(&a, x, y) == 0))
    );
}

#[test]
fn chalk_breaks_up_lighter_paint_more() {
    use crate::brush_engine::brush::BrushType;
    let count = |opacity: f32| {
        let mut chalk = engine(BrushType::Chalk);
        chalk.engines.chalk.grain = 1.0;
        chalk.brush_options.spacing = 60.0;
        chalk.brush_options.opacity = opacity;
        let (c, _) = paint(&mut chalk, &line(64.0, 1.0), 1, true);
        covered(&c)
    };
    let mut soft = engine(BrushType::Soft);
    soft.brush_options.spacing = 60.0;
    let (solid, _) = paint(&mut soft, &line(64.0, 1.0), 1, true);
    assert!(count(1.0) < covered(&solid), "grainy");
    assert!(count(0.3) < count(1.0), "lighter: more broken up");
}

#[test]
fn a_grid_brush_paints_whole_cells_once() {
    use crate::brush_engine::brush::BrushType;
    let mut grid = engine(BrushType::Grid);
    grid.brush_options.diameter = 10.0;
    grid.brush_options.hardness = 100.0;
    grid.engines.grid.cell = 16.0;
    grid.engines.grid.scale = 0.8;
    let forth = line(64.0, 1.0);
    let (once, _) = paint(&mut grid, &forth, 1, true);
    // Cells either side of the line, their middles painted, the gaps
    // between them not.
    assert!(alpha(&once, 40, 56) > 200 && alpha(&once, 40, 72) > 200);
    assert_eq!(alpha(&once, 48, 56), 0, "between cells");
    assert_eq!(alpha(&once, 40, 30), 0, "a cell the brush didn't reach");
    // Over the same cells again: nothing more.
    let mut back = forth.clone();
    back.extend(forth.iter().rev().map(|&(p, t)| (p, t + 1.0)));
    let (twice, _) = paint(&mut grid, &back, 1, true);
    assert!(pixels(&twice) == pixels(&once));
}

#[test]
fn tangent_normal_paints_the_lean_as_its_colour() {
    use crate::brush_engine::brush::BrushType;
    use crate::brush_engine::dynamics::PenTilt;
    let normal_at = |tilt: Option<PenTilt>| {
        let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let mut canvas = Canvas::new(W, H, Color32::WHITE, 64);
        canvas.active_layer_idx = 1;
        let mut b = engine(BrushType::TangentNormal);
        b.brush_options.hardness = 100.0;
        let (mut undo, mut tiles) = (empty_undo(), StrokeTiles::default());
        let mut stroke = StrokeState::with_seed(1);
        {
            let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
            for &(p, t) in &line(64.0, 1.0) {
                stroke.tilt = tilt;
                stroke.add_sample(&mut b, p, 1.0, Some(t), &mut ctx);
            }
            stroke.finish(&mut b, &mut ctx);
        }
        let c = pixel(&canvas, 128, 64);
        [c.r(), c.g(), c.b()]
    };
    let near = |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 3);
    let up = normal_at(Some(PenTilt {
        lean: 0.0,
        direction: 0.0,
    }));
    assert!(near(up, [128, 128, 255]), "{up:?}");
    let right = normal_at(Some(PenTilt {
        lean: 0.8,
        direction: 0.0,
    }));
    assert!(right[0] > 220 && right[2] < 220, "{right:?}");
    // A mouse: leaning the way the stroke goes (right), at 45°.
    let mouse = normal_at(None);
    assert!(near(mouse, [218, 128, 218]), "{mouse:?}");
}

#[test]
fn curve_and_particle_brushes_draw_their_own_repeatable_lines() {
    use crate::brush_engine::brush::BrushType;
    let wavy: Vec<(Vec2, f64)> = (0..=60)
        .map(|i| {
            let t = i as f32 / 60.0;
            (
                Vec2::new(20.0 + t * 216.0, 64.0 + (t * 12.0).sin() * 30.0),
                t as f64,
            )
        })
        .collect();
    for t in [BrushType::Curve, BrushType::Particle] {
        let mut b = engine(t);
        let (a, _) = paint(&mut b, &wavy, 4, true);
        let (again, _) = paint(&mut b, &wavy, 4, true);
        assert!(covered(&a) > 100, "{t:?} paints");
        assert!(pixels(&a) == pixels(&again), "{t:?} repeats");
    }
    // Gravity drags a particle swarm's lines below the pen's.
    let mut heavy = engine(BrushType::Particle);
    heavy.engines.particles.gravity = [0.0, 4.0];
    let (c, _) = paint(&mut heavy, &line(40.0, 1.0), 4, true);
    let below = (41..H)
        .map(|y| (0..W).filter(|&x| alpha(&c, x, y) > 0).count())
        .sum::<usize>();
    let above = (0..40)
        .map(|y| (0..W).filter(|&x| alpha(&c, x, y) > 0).count())
        .sum::<usize>();
    assert!(below > above * 2, "{below} below, {above} above");
}

#[test]
fn inputs_on_one_setting_combine_as_krita_combines_them() {
    use crate::brush_engine::dynamics::{
        Combine, DabSetting, DabVar, InputMapping, Sensor, SensorValues, apply_inputs,
    };
    let s = SensorValues {
        pressure: 0.8,
        speed: 0.25,
        random_dab: 0.75,
        ..Default::default()
    };
    let input = |sensor, setting, amount| InputMapping {
        sensor,
        setting,
        amount,
        ..Default::default()
    };
    let size = [
        input(Sensor::Pressure, DabSetting::Size, 1.0),
        input(Sensor::Speed, DabSetting::Size, 1.0),
    ];
    let scale = |inputs: &[InputMapping], how| {
        let mut v = DabVar::default();
        apply_inputs(inputs, &[(DabSetting::Size, how)], &mut v, &s);
        v.scale
    };
    let near = |a: f32, b: f32| (a - b).abs() < 1e-5;
    // Each on its own (as before): size inputs multiply.
    assert!(near(scale(&size, Combine::Separately), 0.2));
    assert!(near(scale(&size, Combine::Multiply), 0.2));
    assert!(near(scale(&size, Combine::Add), 1.0), "1.05, at most full");
    assert!(near(scale(&size, Combine::Highest), 0.8));
    assert!(near(scale(&size, Combine::Lowest), 0.25));
    assert!(near(scale(&size, Combine::Difference), 0.55));
    // Random is an offset (-1..1) whatever the mode: here +0.5, so the
    // pressure is kept at three quarters.
    let with_random = [
        input(Sensor::Pressure, DabSetting::Size, 1.0),
        input(Sensor::RandomDab, DabSetting::Size, 1.0),
    ];
    assert!(near(scale(&with_random, Combine::Add), 0.8 * 0.75));
    // Rotation: the combined inputs swing it either way by the group's
    // amount (the first input's), the offsets added.
    let turn = |inputs: &[InputMapping], how| {
        let mut v = DabVar::default();
        apply_inputs(inputs, &[(DabSetting::Angle, how)], &mut v, &s);
        v.turn / std::f32::consts::PI
    };
    let angle = [
        input(Sensor::Pressure, DabSetting::Angle, 0.5),
        input(Sensor::Speed, DabSetting::Angle, 0.9),
    ];
    assert!(near(
        turn(&angle, Combine::Lowest),
        0.5 * (2.0 * 0.25 - 1.0)
    ));
    let mut with_offset = angle.to_vec();
    with_offset.push(input(Sensor::RandomDab, DabSetting::Angle, 0.5));
    assert!(near(turn(&with_offset, Combine::Lowest), 0.0), "-0.5 + 0.5");
    // Other settings are left alone.
    let mut v = DabVar::default();
    apply_inputs(&size, &[(DabSetting::Opacity, Combine::Add)], &mut v, &s);
    assert!(near(v.scale, 0.2) && v.strength == 1.0);
}

#[test]
fn scatter_reaches_five_brush_widths_and_mixing_settings_take_inputs() {
    use crate::brush_engine::dynamics::{DabSetting, DabVar, SensorValues, apply_inputs};
    let s = SensorValues {
        pressure: 0.5,
        ..Default::default()
    };
    let mut v = DabVar::default();
    let inputs = [
        map_pressure(DabSetting::Scatter, 4.0),
        map_pressure(DabSetting::SmudgeLength, 1.0),
        map_pressure(DabSetting::ColorRate, 1.0),
    ];
    apply_inputs(&inputs, &[], &mut v, &s);
    assert_eq!((v.scatter, v.smudge, v.color_rate), (2.0, 0.5, 0.5));
}

#[test]
fn a_grid_that_repaints_builds_its_cells_up() {
    use crate::brush_engine::brush::BrushType;
    let mut grid = engine(BrushType::Grid);
    grid.brush_options.diameter = 10.0;
    grid.brush_options.hardness = 100.0;
    grid.brush_options.opacity = 0.3;
    grid.engines.grid.cell = 16.0;
    let (once, _) = paint(&mut grid, &line(64.0, 1.0), 1, true);
    grid.engines.grid.repaint = true;
    let (built, _) = paint(&mut grid, &line(64.0, 1.0), 1, true);
    assert!(alpha(&built, 40, 56) > alpha(&once, 40, 56) + 20);
    // Tall cells: the shapes reach further up and down than across.
    grid.engines.grid.repaint = false;
    grid.engines.grid.cell_height = 48.0;
    grid.brush_options.opacity = 1.0;
    let (tall, _) = paint(&mut grid, &line(64.0, 1.0), 1, true);
    let column = |x| (0..H).filter(|&y| alpha(&tall, x, y) > 0).count();
    let row = |y| (0..W).filter(|&x| alpha(&tall, x, y) > 0).count();
    assert!(column(40) > 30, "tall shapes: {}", column(40));
    assert!(row(72) > 100, "still along the whole line: {}", row(72));
}

#[test]
fn light_pressure_lifts_bristles_and_coverage_grows_a_spray() {
    use crate::brush_engine::brush::BrushType;
    let mut b = bristle_brush(0.0);
    b.bristles.count = 40;
    b.bristles.pressure_cut = 1.0;
    let pts = |p: f32| {
        (0..60)
            .map(|i| (Vec2::new(20.0 + i as f32 * 3.0, 64.0), p))
            .collect::<Vec<_>>()
    };
    let light = covered(&stroke_with_pressure(&mut b, &pts(0.2)));
    let hard = covered(&stroke_with_pressure(&mut b, &pts(1.0)));
    assert!(light * 2 < hard, "{light} vs {hard}");

    // By coverage, a bigger spray gets more particles: it covers the same
    // share of a bigger area.
    let mut spray = engine(BrushType::Spray);
    spray.engines.spray.coverage = 0.2;
    spray.brush_options.diameter = 20.0;
    let (small, _) = paint(&mut spray, &[(Vec2::new(60.0, 64.0), 0.0)], 3, true);
    spray.brush_options.diameter = 60.0;
    let (big, _) = paint(&mut spray, &[(Vec2::new(60.0, 64.0), 0.0)], 3, true);
    assert!(
        covered(&big) > 4 * covered(&small),
        "{} vs {}",
        covered(&big),
        covered(&small)
    );
}
