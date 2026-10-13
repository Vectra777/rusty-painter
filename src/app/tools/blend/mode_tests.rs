use super::{DeformMode, FilterMode, SmudgeMode};
use crate::canvas::Canvas;
use eframe::egui::{Color32, Vec2};

/// A 128×64 layer: `paint(x, y)` for each pixel.
fn app(paint: impl Fn(i32, i32) -> Color32) -> crate::PainterApp {
    let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
    app.canvas_mut().active_layer_idx = 1;
    for tx in 0..2 {
        let tile = (0..64 * 64)
            .map(|i| paint(tx * 64 + i % 64, i / 64))
            .collect();
        app.canvas_mut().set_layer_tile_data(1, tx, 0, tile);
    }
    let o = &mut app.brush_state.brush.brush_options;
    o.diameter = 30.0;
    o.hardness = 100.0;
    o.flow = 100.0;
    o.opacity = 1.0;
    o.pressure_size = false;
    o.spacing = 10.0;
    app
}

fn px(app: &crate::PainterApp, x: i32, y: i32) -> Color32 {
    app.canvas
        .get_layer_tile_data(1, x / 64, y / 64)
        .map_or(Color32::TRANSPARENT, |t| {
            t[((y % 64) * 64 + x % 64) as usize]
        })
}

fn all(app: &crate::PainterApp) -> Vec<Color32> {
    (0..64)
        .flat_map(|y| (0..128).map(move |x| (x, y)))
        .map(|(x, y)| px(app, x, y))
        .collect()
}

fn stroke(app: &mut crate::PainterApp, from: Vec2, to: Vec2) {
    app.blend_press(from, 1.0);
    for i in 1..=10 {
        app.blend_drag(from + (to - from) * (i as f32 / 10.0), 1.0);
    }
    app.blend_release();
    app.settle_strokes();
}

fn undo(app: &mut crate::PainterApp) {
    let mut tool = app.active_tool;
    let canvas = std::sync::Arc::get_mut(&mut app.canvas).unwrap();
    app.layer_state
        .history
        .undo(canvas, &mut app.selection_manager, &mut tool);
}

fn dot(x: i32, y: i32) -> Color32 {
    if (x - 64).pow(2) + (y - 32).pow(2) <= 36 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn dark(app: &crate::PainterApp) -> usize {
    all(app).iter().filter(|c| c.r() < 128).count()
}

#[test]
fn deform_grows_shrinks_and_undoes_exactly() {
    let deformed = |mode, amount| {
        let mut a = app(dot);
        a.active_tool = crate::app::tools::Tool::Smudge;
        a.workspace.blend.smudge_mode = SmudgeMode::Deform;
        a.workspace.blend.deform_mode = mode;
        a.workspace.blend.deform_amount = amount;
        let c = Vec2::new(64.0, 32.0);
        stroke(&mut a, c, c + Vec2::new(0.5, 0.0));
        a
    };
    let before = dark(&app(dot));
    let still = deformed(DeformMode::Grow, 0.0);
    assert_eq!(all(&still), all(&app(dot)), "no amount, no change");
    assert!(dark(&deformed(DeformMode::Grow, 0.8)) > before * 3 / 2);
    assert!(dark(&deformed(DeformMode::Shrink, 0.8)) < before * 2 / 3);
    let mut grown = deformed(DeformMode::Grow, 0.8);
    undo(&mut grown);
    assert_eq!(all(&grown), all(&app(dot)));
}

#[test]
fn deform_pushes_the_paint_along_the_stroke() {
    // A black bar at x 40..44, pushed right.
    let bar = |x: i32, _| {
        if (40..44).contains(&x) {
            Color32::BLACK
        } else {
            Color32::WHITE
        }
    };
    let mut a = app(bar);
    a.active_tool = crate::app::tools::Tool::Smudge;
    a.workspace.blend.smudge_mode = SmudgeMode::Deform;
    a.workspace.blend.deform_mode = DeformMode::Push;
    a.workspace.blend.deform_amount = 1.0;
    stroke(&mut a, Vec2::new(30.0, 32.0), Vec2::new(90.0, 32.0));
    // At full amount the bar keeps up with the brush: from 40 to 100.
    let dark: Vec<i32> = (30..128).filter(|&x| px(&a, x, 32).r() < 128).collect();
    assert!(
        !dark.is_empty() && dark.iter().all(|&x| (96..108).contains(&x)),
        "{dark:?}"
    );
    // Rows away from the stroke keep the bar where it was.
    assert!(px(&a, 42, 2).r() < 50 && px(&a, 60, 2).r() > 200);
}

#[test]
fn clone_copies_the_source_pixel_for_pixel() {
    let pattern = |x: i32, y: i32| Color32::from_rgb((x * 7 % 256) as u8, (y * 11 % 256) as u8, 90);
    let mut a = app(pattern);
    a.active_tool = crate::app::tools::Tool::Smudge;
    a.workspace.blend.smudge_mode = SmudgeMode::Clone;
    // Nothing happens before a source is set.
    stroke(&mut a, Vec2::new(90.0, 32.0), Vec2::new(100.0, 32.0));
    assert_eq!(a.layer_state.history.push_count(), 0);
    a.workspace.blend.set_clone_source(Vec2::new(30.0, 32.0));
    stroke(&mut a, Vec2::new(90.0, 32.0), Vec2::new(100.0, 32.0));
    for x in 88..102 {
        let (got, want) = (px(&a, x, 32), pattern(x - 60, 32));
        for (g, w) in got.to_array().iter().zip(want.to_array()) {
            assert!(g.abs_diff(w) <= 1, "x {x}: {got:?} vs {want:?}");
        }
    }
    // Aligned: the next stroke keeps the offset.
    stroke(&mut a, Vec2::new(70.0, 20.0), Vec2::new(71.0, 20.0));
    let (got, want) = (px(&a, 70, 20), pattern(10, 20));
    assert!(got.r().abs_diff(want.r()) <= 1, "{got:?} vs {want:?}");
    undo(&mut a);
    undo(&mut a);
    assert_eq!(all(&a), all(&app(pattern)));
}

#[test]
fn with_wrap_around_blur_mixes_across_the_edge() {
    // Black on the left edge, white elsewhere; blurred at the edge.
    let left = |x: i32, _| {
        if x < 4 {
            Color32::BLACK
        } else {
            Color32::WHITE
        }
    };
    let blurred = |wrap: bool| {
        let mut a = app(left);
        a.workspace.wrap_around = wrap;
        a.active_tool = crate::app::tools::Tool::Blur;
        a.workspace.blend.blur_size = 0.5;
        stroke(&mut a, Vec2::new(1.0, 20.0), Vec2::new(1.0, 44.0));
        a
    };
    let (wrapped, plain) = (blurred(true), blurred(false));
    // The thin band spreads out evenly both sides of the edge.
    assert!(px(&wrapped, 126, 32).r() < 250, "darkened across the edge");
    assert!(px(&wrapped, 126, 32).r().abs_diff(px(&wrapped, 5, 32).r()) <= 3);
    assert_eq!(px(&plain, 126, 32), Color32::WHITE);
    // One undo step takes both sides back.
    let mut wrapped = wrapped;
    undo(&mut wrapped);
    assert_eq!(all(&wrapped), all(&app(left)));
}

#[test]
fn sharpen_strengthens_an_edge_and_adjust_turns_the_hue() {
    let edge = |x: i32, _| {
        if x < 64 {
            Color32::from_gray(90)
        } else {
            Color32::from_gray(170)
        }
    };
    let mut a = app(edge);
    a.active_tool = crate::app::tools::Tool::Blur;
    a.workspace.blend.filter_mode = FilterMode::Sharpen;
    a.workspace.blend.sharpen_amount = 1.5;
    stroke(&mut a, Vec2::new(64.0, 20.0), Vec2::new(64.0, 44.0));
    assert!(
        px(&a, 62, 32).r() < 90,
        "darker beside the edge: {:?}",
        px(&a, 62, 32)
    );
    assert!(
        px(&a, 65, 32).r() > 170,
        "lighter beside it: {:?}",
        px(&a, 65, 32)
    );

    let red = |_, _| Color32::from_rgb(220, 30, 30);
    let mut a = app(red);
    a.active_tool = crate::app::tools::Tool::Blur;
    a.workspace.blend.filter_mode = FilterMode::Adjust;
    a.workspace.blend.adjust_hue = 120.0;
    stroke(&mut a, Vec2::new(40.0, 32.0), Vec2::new(80.0, 32.0));
    let c = px(&a, 60, 32);
    assert!(c.g() > 150 && c.r() < 80, "red turned green: {c:?}");
}
fn filter_brush(
    paint: impl Fn(i32, i32) -> Color32,
    filter: crate::canvas::filters::Filter,
) -> crate::PainterApp {
    let mut a = app(paint);
    a.active_tool = crate::app::tools::Tool::Blur;
    a.workspace.blend.filter_mode = FilterMode::Filter;
    a.workspace.blend.brush_filter = filter;
    a
}

fn checks(x: i32, y: i32) -> Color32 {
    if (x / 4 + y / 4) % 2 == 0 {
        Color32::from_rgb(200, 40, 30)
    } else {
        Color32::from_rgb(20, 90, 230)
    }
}

#[test]
fn the_filter_brush_changes_only_what_it_covers_and_undoes_exactly() {
    use crate::canvas::filters::Filter;
    let mut a = filter_brush(checks, Filter::Invert);
    let before = all(&a);
    // There and back in one stroke: going over a spot again doesn't
    // invert it back (nothing is filtered twice).
    a.blend_press(Vec2::new(30.0, 32.0), 1.0);
    for i in 1..=20 {
        let t = if i <= 10 { i } else { 20 - i } as f32 / 10.0;
        a.blend_drag(Vec2::new(30.0 + 60.0 * t, 32.0), 1.0);
    }
    a.blend_release();
    a.settle_strokes();
    assert_eq!(a.layer_state.history.push_count(), 1, "one undo step");
    let inverted = |c: Color32| Color32::from_rgb(255 - c.r(), 255 - c.g(), 255 - c.b());
    for y in 0..64 {
        for x in 0..128 {
            let got = px(&a, x, y);
            let was = before[(y * 128 + x) as usize];
            // The brush: 30 px across along y = 32, from x 30 to 90.
            let dx = ((x - 60).abs() - 30).max(0);
            let d = ((dx * dx + (y - 32) * (y - 32)) as f32).sqrt();
            if d > 16.0 {
                assert_eq!(got, was, "({x}, {y}) is outside the brush");
            } else if d < 12.0 {
                let want = inverted(was);
                for (g, w) in got.to_array().iter().zip(want.to_array()) {
                    assert!(g.abs_diff(w) <= 1, "({x}, {y}): {got:?} vs {want:?}");
                }
            }
        }
    }
    undo(&mut a);
    assert_eq!(all(&a), before);
}

#[test]
fn the_filter_brush_reads_the_layer_as_it_was_before_the_stroke() {
    use crate::canvas::filters::Filter;
    let blur = Filter::GaussianBlur { radius: 3.0 };
    let mut a = filter_brush(checks, blur);
    let before = all(&a);
    // Back and forth, dabs overlapping many times.
    stroke(&mut a, Vec2::new(30.0, 32.0), Vec2::new(90.0, 32.0));
    let want = blur.apply(&before, 128, 64, (0, 0));
    let mut changed = 0;
    for x in 35..85 {
        for y in 26..38 {
            let (got, w) = (px(&a, x, y), want[(y * 128 + x) as usize]);
            for (g, v) in got.to_array().iter().zip(w.to_array()) {
                assert!(g.abs_diff(v) <= 2, "({x}, {y}): {got:?} vs {w:?}");
            }
            changed += usize::from(got != before[(y * 128 + x) as usize]);
        }
    }
    assert!(changed > 400, "blurred: {changed}");
}

#[test]
fn every_filter_paints_through_the_brush() {
    use crate::canvas::filters::Filter;
    for &filter in Filter::MENU.iter().flat_map(|g| g.iter()) {
        // A two-colour checkerboard is already its own median, at any
        // radius (the median's own tests cover it).
        if matches!(filter, Filter::Median { .. }) {
            continue;
        }
        let filter = match filter {
            // Its defaults change nothing.
            Filter::BrightnessContrast { .. } => Filter::BrightnessContrast {
                brightness: 0.4,
                contrast: 0.2,
            },
            Filter::HueSaturation { .. } => Filter::HueSaturation {
                hue: 90.0,
                saturation: 0.0,
                lightness: 0.0,
            },
            Filter::Levels { .. } => Filter::Levels {
                black: 0.2,
                white: 0.8,
                gamma: 1.0,
            },
            Filter::Curves { .. } => {
                let invert =
                    crate::canvas::filters::ToneCurve::from_points(&[[0.0, 1.0], [1.0, 0.0]]);
                let same = crate::canvas::filters::ToneCurve::default();
                Filter::Curves {
                    rgb: invert,
                    red: same,
                    green: same,
                    blue: same,
                }
            }
            Filter::Exposure { .. } => Filter::Exposure { stops: 1.0 },
            // The checks are too dark to glow from the default brightness.
            Filter::Glow { radius, .. } => Filter::Glow {
                radius,
                strength: 1.0,
                threshold: 0.0,
            },
            Filter::Temperature { .. } => Filter::Temperature {
                temperature: 0.8,
                tint: 0.3,
            },
            Filter::Vibrance { .. } => Filter::Vibrance { amount: -1.0 },
            // Reaching the stroke in the middle of the canvas.
            Filter::Vignette { frame, .. } => Filter::Vignette {
                amount: 1.0,
                size: 0.0,
                frame,
            },
            Filter::ZoomBlur { frame, .. } => Filter::ZoomBlur { amount: 0.5, frame },
            Filter::SpinBlur { frame, .. } => Filter::SpinBlur { angle: 60.0, frame },
            Filter::ColourBalance { .. } => Filter::ColourBalance {
                shadows: [0.8, 0.0, 0.0],
                midtones: [0.8, 0.0, 0.0],
                highlights: [0.0, 0.0, -0.8],
                preserve_luminosity: false,
            },
            f => f,
        };
        let mut a = filter_brush(checks, filter);
        let before = all(&a);
        stroke(&mut a, Vec2::new(30.0, 32.0), Vec2::new(90.0, 32.0));
        assert!(all(&a) != before, "{}", filter.name());
        assert_eq!(px(&a, 5, 5), before[5 * 128 + 5], "{}", filter.name());
        undo(&mut a);
        assert!(all(&a) == before, "{} undoes", filter.name());
    }
}
