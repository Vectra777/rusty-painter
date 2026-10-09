//! The airbrush, several tips per brush, and the dual brush.

use super::*;

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
fn round_trip_tips_go_there_and_back() {
    use crate::brush_engine::brush_options::TipOrder;
    let used = tips_used(
        &mut multi_tip(TipOrder::RoundTrip),
        &along_x(40.0, 200.0, 20),
        1,
    );
    assert!(used.len() >= 6);
    for (i, &t) in used.iter().enumerate() {
        assert_eq!(t as usize, [0, 1, 2, 1][i % 4], "{used:?}");
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
    // Burn leaves solid paint solid whatever the mask.
    for mode in DualMode::ALL.into_iter().filter(|&m| m != DualMode::Burn) {
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
