//! Wet edges, bristles, colour tips, ribbons, hatching, sketching and wrap-around.

use super::*;

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

#[test]
fn sketch_lines_thin_out_and_shorten_by_their_inputs() {
    use crate::brush_engine::dynamics::DabSetting;
    let zigzag: Vec<(Vec2, f32)> = (0..=60)
        .map(|i| {
            let t = i as f32 / 60.0;
            let y = if i % 20 < 10 {
                40.0 + (i % 10) as f32 * 5.0
            } else {
                90.0 - (i % 10) as f32 * 5.0
            };
            (Vec2::new(40.0 + 170.0 * t, y), 0.2)
        })
        .collect();
    let sketch = || {
        let mut b = Brush::new(2.0, 90.0, Color32::BLACK, 20.0);
        b.brush_options.pressure_size = false;
        b.brush_type = crate::brush_engine::brush::BrushType::Sketch;
        b.sketch.density = 1.0;
        b.sketch.opacity = 1.0;
        b.sketch.thickness = 4.0;
        b
    };
    let covered = |c: &Canvas| pixels(c).iter().filter(|&&a| a > 20).count();
    let ink = |c: &Canvas| pixels(c).iter().map(|&a| a as u64).sum::<u64>();
    let full = stroke_with_pressure(&mut sketch(), &zigzag);
    // Light pressure (a fifth) driving the line width: as thin as lines a
    // fifth as wide.
    let mut thin = sketch();
    thin.inputs = vec![map_pressure(DabSetting::SketchWidth, 1.0)];
    let thin = stroke_with_pressure(&mut thin, &zigzag);
    let mut narrow = sketch();
    narrow.sketch.thickness = 0.8;
    let narrow = stroke_with_pressure(&mut narrow, &zigzag);
    assert!(covered(&thin) < covered(&full) * 3 / 4);
    assert_eq!(pixels(&thin), pixels(&narrow));
    // Left off at both ends: less ink; by pressure, a fifth of that.
    let mut short = sketch();
    short.sketch.offset = 0.3;
    let mut by_pressure = short.clone();
    by_pressure.inputs = vec![map_pressure(DabSetting::SketchOffset, 1.0)];
    let mut fifth = short.clone();
    fifth.sketch.offset = 0.06;
    let short = stroke_with_pressure(&mut short, &zigzag);
    assert!(ink(&short) < ink(&full));
    assert_eq!(
        pixels(&stroke_with_pressure(&mut by_pressure, &zigzag)),
        pixels(&stroke_with_pressure(&mut fifth, &zigzag))
    );
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
