use super::{Codec, halton_dull};
use crate::canvas::Canvas;
use eframe::egui::{Color32, Vec2};

fn app(below: Option<Color32>) -> crate::PainterApp {
    let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
    app.canvas_mut().active_layer_idx = 1;
    if let Some(c) = below {
        for tx in 0..2 {
            app.canvas_mut()
                .set_layer_tile_data(1, tx, 0, vec![c; 64 * 64]);
        }
    }
    app.active_tool = crate::app::tools::Tool::Smudge;
    let o = &mut app.brush_state.brush.brush_options;
    o.diameter = 20.0;
    o.hardness = 100.0;
    o.pressure_size = false;
    o.color = Color32::from_rgb(20, 40, 230);
    app
}

fn drag(app: &mut crate::PainterApp) {
    app.blend_press(Vec2::new(10.0, 32.0), 1.0);
    for i in 1..=20 {
        app.blend_drag(Vec2::new(10.0 + i as f32 * 5.0, 32.0), 1.0);
    }
    app.blend_release();
    app.settle_strokes();
}

fn pixel(app: &crate::PainterApp, x: i32) -> Color32 {
    app.canvas
        .get_layer_tile_data(1, x / 64, 0)
        .map_or(Color32::TRANSPARENT, |t| t[(32 * 64 + x % 64) as usize])
}

#[test]
fn a_wet_brush_paints_its_colour_on_empty_layers() {
    let mut app = app(None);
    app.workspace.blend.color_rate = 0.6;
    drag(&mut app);
    let p = pixel(&app, 60);
    assert!(
        p.a() > 200 && p.b() > p.r() * 3,
        "brush blue laid down: {p:?}"
    );
}

#[test]
fn a_wet_brush_mixes_with_the_paint_under_it() {
    let red = Color32::from_rgb(230, 30, 20);
    let mut app = app(Some(red));
    app.workspace.blend.color_rate = 0.3;
    drag(&mut app);
    let p = pixel(&app, 60);
    assert!(p.r() > 40 && p.b() > 40, "red and blue mixed: {p:?}");
}

#[test]
fn the_colour_rate_doesnt_depend_on_spacing() {
    let red = Color32::from_rgb(230, 30, 20);
    let at = |spacing: f32| {
        let mut app = app(Some(red));
        app.workspace.blend.color_rate = 0.3;
        app.brush_state.brush.brush_options.spacing = spacing;
        drag(&mut app);
        pixel(&app, 60)
    };
    let (dense, sparse) = (at(10.0), at(40.0));
    let diff = dense
        .to_array()
        .iter()
        .zip(sparse.to_array())
        .map(|(a, b)| a.abs_diff(b))
        .max()
        .unwrap();
    assert!(diff <= 40, "{dense:?} vs {sparse:?}");
}

#[test]
fn pressure_blends_along_a_blur_stroke() {
    // Hard stripes, blurred by a pen whose pressure rises from light to
    // full over one long segment: the blurred band widens gradually.
    let mut app = app(None);
    let stripes: Vec<Color32> = (0..64 * 64)
        .map(|i| {
            if (i % 64) % 4 < 2 {
                Color32::BLACK
            } else {
                Color32::WHITE
            }
        })
        .collect();
    for tx in 0..2 {
        app.canvas_mut()
            .set_layer_tile_data(1, tx, 0, stripes.clone());
    }
    let before: Vec<Color32> = (0..128)
        .flat_map(|x| (0..64).map(move |y| (x, y)))
        .map(|(x, y)| px(&app, x, y))
        .collect();
    app.active_tool = crate::app::tools::Tool::Blur;
    let o = &mut app.brush_state.brush.brush_options;
    o.diameter = 40.0;
    o.pressure_size = true;
    o.pressure_min_size = 0.1;
    o.spacing = 5.0;
    app.blend_press(Vec2::new(10.0, 32.0), 0.1);
    app.blend_drag(Vec2::new(118.0, 32.0), 1.0);
    app.blend_release();
    app.settle_strokes();
    let band = |x: i32| {
        (0..64)
            .filter(|&y| px(&app, x, y) != before[(x * 64 + y) as usize])
            .count() as i32
    };
    let widths: Vec<i32> = (20..100).step_by(4).map(band).collect();
    let jump = widths
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .max()
        .unwrap();
    assert!(
        widths[widths.len() - 1] > widths[0] + 10,
        "widens: {widths:?}"
    );
    assert!(jump <= 4, "no steps: {widths:?}");
}

fn px(app: &crate::PainterApp, x: i32, y: i32) -> Color32 {
    app.canvas
        .get_layer_tile_data(1, x / 64, y / 64)
        .map_or(Color32::TRANSPARENT, |t| {
            t[((y % 64) * 64 + x % 64) as usize]
        })
}

/// Every pixel of layer 1.
fn layer(app: &crate::PainterApp) -> Vec<Color32> {
    (0..64)
        .flat_map(|y| (0..128).map(move |x| (x, y)))
        .map(|(x, y)| {
            app.canvas
                .get_layer_tile_data(1, x / 64, 0)
                .map_or(Color32::TRANSPARENT, |t| t[(y * 64 + x % 64) as usize])
        })
        .collect()
}

/// A layer of every colour and many alphas (premultiplied, as stored).
fn varied(app: &mut crate::PainterApp) {
    for tx in 0..2 {
        let tile = (0..64 * 64)
            .map(|i| {
                let (x, y) = (tx * 64 + i % 64, i / 64);
                Color32::from_rgba_unmultiplied(
                    (x * 2) as u8,
                    (y * 4) as u8,
                    ((x * 7 + y * 3) % 256) as u8,
                    (40 + (x + y * 3) % 216) as u8,
                )
            })
            .collect();
        app.canvas_mut().set_layer_tile_data(1, tx, 0, tile);
    }
}

#[test]
fn smudging_leaves_every_pixel_outside_the_brush_exactly_as_it_was() {
    for color_rate in [0.0, 0.5] {
        let mut app = app(None);
        varied(&mut app);
        app.workspace.blend.color_rate = color_rate;
        let before = layer(&app);
        drag(&mut app);
        let after = layer(&app);
        // The stroke: y = 32, x 10..110, a 20 px brush.
        let mut inside = 0;
        for (i, (b, a)) in before.iter().zip(&after).enumerate() {
            let (x, y) = ((i % 128) as f32 + 0.5, (i / 128) as f32 + 0.5);
            let dx = (x - x.clamp(10.0, 110.0)).abs();
            let d = (dx * dx + (y - 32.0) * (y - 32.0)).sqrt();
            if d > 11.0 {
                assert_eq!(a, b, "({x}, {y}) is outside the brush ({color_rate})");
            } else if a != b {
                inside += 1;
            }
        }
        assert!(inside > 500, "smudged: {inside} ({color_rate})");
    }
}

#[test]
fn a_mixing_brush_paints_what_the_smudge_tool_does() {
    use crate::brush_engine::brush_options::Mixing;
    let red = Color32::from_rgb(230, 30, 20);
    let mut tool = app(Some(red));
    tool.workspace.blend.smudge_length = 0.6;
    tool.workspace.blend.color_rate = 0.3;
    drag(&mut tool);
    let mut brush = app(Some(red));
    brush.active_tool = crate::app::tools::Tool::Brush;
    brush.brush_state.brush.mixing = Some(Mixing {
        smudge_length: 0.6,
        color_rate: 0.3,
        ..Default::default()
    });
    brush.start_stroke_with_pressure(Vec2::new(10.0, 32.0), 1.0);
    for i in 1..=20 {
        brush.add_stroke_point(Vec2::new(10.0 + i as f32 * 5.0, 32.0), 1.0);
    }
    brush.finish_stroke();
    brush.settle_strokes();
    assert!(
        layer(&tool) == layer(&brush),
        "the same engine, the same pixels"
    );
}

/// A mixing brush that lays down its own colour, nothing carried.
fn pure_mixing(app: &mut crate::PainterApp) {
    use crate::brush_engine::brush_options::Mixing;
    app.active_tool = crate::app::tools::Tool::Brush;
    app.brush_state.brush.mixing = Some(Mixing {
        smudge_length: 0.0,
        color_rate: 1.0,
        ..Default::default()
    });
}

/// One dab at (60, 32) with the Brush tool.
fn dab(app: &mut crate::PainterApp) -> Vec<Color32> {
    app.start_stroke_with_pressure(Vec2::new(60.0, 32.0), 1.0);
    app.finish_stroke();
    app.settle_strokes();
    layer(app)
}

fn painted(px: &[Color32], x: i32, y: i32) -> bool {
    px[(y * 128 + x) as usize] != Color32::WHITE
}

#[test]
fn a_mixing_brush_s_colour_rate_follows_its_inputs() {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    // Its own colour at full rate, unless an input (pressure) eases
    // it: lightly pressed, it lays down less of it.
    let painted_at = |pressure: f32| {
        let mut app = app(Some(Color32::WHITE));
        pure_mixing(&mut app);
        app.brush_state.brush.inputs = vec![InputMapping {
            sensor: Sensor::Pressure,
            setting: DabSetting::ColorRate,
            ..Default::default()
        }];
        app.start_stroke_with_pressure(Vec2::new(10.0, 32.0), pressure);
        for i in 1..=10 {
            app.add_stroke_point(Vec2::new(10.0 + i as f32 * 5.0, 32.0), pressure);
        }
        app.finish_stroke();
        app.settle_strokes();
        let px = layer(&app);
        // How far from white the middle of the line is.
        let c = px[32 * 128 + 35];
        765 - (c.r() as i32 + c.g() as i32 + c.b() as i32)
    };
    assert!(
        painted_at(0.2) < painted_at(1.0),
        "{} {}",
        painted_at(0.2),
        painted_at(1.0)
    );
}

#[test]
fn a_lightness_tip_lays_relief_that_undoes_saves_and_bakes() {
    use crate::brush_engine::brush_options::{KritaSmudge, Mixing, PixelBrushShape, TipMapping};
    // A colour tip: dark on its top half, light on its bottom (along
    // the stroke, each keeps its own side of the line).
    let side = 16;
    let colors: Vec<[u8; 3]> = (0..side * side)
        .map(|i| {
            if i / side < side / 2 {
                [40; 3]
            } else {
                [220; 3]
            }
        })
        .collect();
    let tip =
        crate::brush_engine::tip::TipMask::from_colored(side, side, vec![255; side * side], colors);
    let paint = Color32::from_rgb(200, 120, 40);
    let mut a = app(Some(paint));
    a.active_tool = crate::app::tools::Tool::Brush;
    let b = &mut a.brush_state.brush;
    b.brush_options.pixel_shape = PixelBrushShape::Custom(tip);
    b.brush_options.tip_mapping = TipMapping::Lightness;
    b.brush_options.color = paint;
    b.mixing = Some(Mixing {
        smudge_length: 0.5,
        color_rate: 0.0,
        krita: Some(KritaSmudge::default()),
        ..Default::default()
    });
    assert!(b.lays_lightness());
    let before = a.canvas.flatten().pixels;
    let pushes = a.layer_state.history.push_count();
    a.start_stroke_with_pressure(Vec2::new(10.0, 32.0), 1.0);
    for i in 1..=20 {
        a.add_stroke_point(Vec2::new(10.0 + i as f32 * 5.0, 32.0), 1.0);
    }
    a.finish_stroke();
    a.settle_strokes();
    assert_eq!(a.layer_state.history.push_count(), pushes + 1, "one step");
    let layer = &a.canvas.layers[1];
    assert!(layer.style.lightness_map);
    assert!(layer.height.as_deref().is_some_and(|h| !h.is_empty()));
    // The colour is one colour still; what shows has light and shade.
    assert!(layer_px(&a).iter().all(|&c| c == paint), "colour unchanged");
    let shown = a.canvas.flatten().pixels;
    let light = |c: &Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
    let (above, below) = (light(&shown[28 * 128 + 60]), light(&shown[36 * 128 + 60]));
    assert!(below > above + 60, "relief: {above} above, {below} below");
    // Saved and opened: the same.
    let bytes = crate::project::encode_project(&a).unwrap();
    assert!(
        crate::project::decode_project(&bytes)
            .unwrap()
            .canvas
            .flatten()
            .pixels
            == shown
    );
    // Undone: flat again; redone: back.
    a.apply_history(false);
    assert!(a.canvas.flatten().pixels == before);
    a.apply_history(true);
    assert!(a.canvas.flatten().pixels == shown);
    // An impasto brush there: the relief goes into the paint first,
    // looking the same.
    a.ensure_impasto(1);
    assert!(!a.canvas.layers[1].style.lightness_map);
    let baked = a.canvas.flatten().pixels;
    let off = (0..baked.len())
        .filter(|&i| {
            baked[i]
                .to_array()
                .iter()
                .zip(shown[i].to_array())
                .any(|(x, y)| x.abs_diff(y) > 1)
        })
        .count();
    assert_eq!(off, 0, "baked as it showed");
}

/// Layer 1's pixels where the stroke went (y 28..36).
fn layer_px(app: &crate::PainterApp) -> Vec<Color32> {
    (28..36)
        .flat_map(|y| (0..128).map(move |x| (x, y)))
        .map(|(x, y)| px(app, x, y))
        .collect()
}

#[test]
fn a_mixing_brush_turns_and_squashes_its_tip() {
    let extents = |angle: f32| {
        let mut app = app(Some(Color32::WHITE));
        pure_mixing(&mut app);
        let tip = &mut app.brush_state.brush.dynamics.tip;
        (tip.ratio, tip.angle) = (0.3, angle);
        let px = dab(&mut app);
        let across = (40..80).filter(|&x| painted(&px, x, 32)).count();
        let down = (12..52).filter(|&y| painted(&px, 60, y)).count();
        (across, down)
    };
    let (across, down) = extents(0.0);
    let (turned_across, turned_down) = extents(90.0);
    assert!(across != down, "squashed: {across} × {down}");
    assert_eq!(
        (turned_across, turned_down),
        (down, across),
        "turned a quarter"
    );
}

#[test]
fn a_mixing_brush_takes_its_texture_and_its_hue_randomness() {
    let full = |px: &[Color32]| {
        px.iter()
            .filter(|&&c| c == Color32::from_rgb(20, 40, 230))
            .count()
    };
    let mut plain = app(Some(Color32::WHITE));
    pure_mixing(&mut plain);
    let plain_px = dab(&mut plain);
    let mut textured = app(Some(Color32::WHITE));
    pure_mixing(&mut textured);
    let mut t = crate::brush_engine::texture::BrushTexture::new(
        crate::brush_engine::texture::builtin()[1].clone(),
    );
    t.strength = 1.0;
    textured.brush_state.brush.texture = Some(t);
    let textured_px = dab(&mut textured);
    assert!(
        full(&textured_px) < full(&plain_px),
        "the grain shows through"
    );
    assert!(textured_px.iter().any(|&c| c != Color32::WHITE));
    // Hue randomness: the colour mixed in turns from the brush's.
    let mut hued = app(Some(Color32::WHITE));
    pure_mixing(&mut hued);
    hued.brush_state.brush.dynamics.random.hue = 120.0;
    hued.start_stroke_with_pressure(Vec2::new(10.0, 32.0), 1.0);
    for i in 1..=20 {
        hued.add_stroke_point(Vec2::new(10.0 + i as f32 * 5.0, 32.0), 1.0);
    }
    hued.finish_stroke();
    hued.settle_strokes();
    let px = layer(&hued);
    assert!(
        px.iter().any(|c| c.r() > 60 || c.g() > 80),
        "some dab isn't the brush's blue"
    );
}

#[test]
fn a_mixing_brush_scatters_within_its_jitter() {
    let mut app = app(Some(Color32::WHITE));
    pure_mixing(&mut app);
    app.brush_state.brush.brush_options.diameter = 6.0;
    // ±100% of the diameter.
    app.brush_state.brush.jitter = 100.0;
    app.start_stroke_with_pressure(Vec2::new(10.0, 32.0), 1.0);
    for i in 1..=20 {
        app.add_stroke_point(Vec2::new(10.0 + i as f32 * 5.0, 32.0), 1.0);
    }
    app.finish_stroke();
    app.settle_strokes();
    let px = layer(&app);
    let off_line = (0..64)
        .filter(|&y| (y - 32i32).abs() > 4)
        .any(|y| (0..128).any(|x| painted(&px, x, y)));
    assert!(off_line, "scattered off the line");
    let far = (0..64)
        .filter(|&y| (y - 32i32).abs() > 14)
        .any(|y| (0..128).any(|x| painted(&px, x, y)));
    assert!(!far, "but not past the jitter");
}

#[test]
fn a_mixing_brush_follows_its_stabiliser_and_post_correction_is_one_step() {
    use crate::brush_engine::brush::StabilizerAlgorithm;
    // A pulled string longer than the drag: the brush stays put.
    let mut held = app(Some(Color32::WHITE));
    pure_mixing(&mut held);
    held.brush_state.brush.stabilizer_algorithm = StabilizerAlgorithm::String;
    held.brush_state.brush.stabilizer_modes.string_length = 60.0;
    held.brush_state.brush.stabilizer_modes.catch_up = false;
    held.start_stroke_with_pressure(Vec2::new(10.0, 32.0), 1.0);
    for i in 1..=8 {
        held.add_stroke_point(Vec2::new(10.0 + i as f32 * 5.0, 32.0), 1.0);
    }
    held.finish_stroke();
    held.settle_strokes();
    let px = layer(&held);
    assert!(painted(&px, 10, 32) && !painted(&px, 45, 32));
    // Post-correction: a wobbly line comes out smoother, one undo step
    // that takes it all back.
    let wobbly = |app: &mut crate::PainterApp| {
        app.start_stroke_with_pressure(Vec2::new(10.0, 32.0), 1.0);
        for i in 1..=30 {
            let y = 32.0 + if i % 2 == 0 { 5.0 } else { -5.0 };
            app.add_stroke_point(Vec2::new(10.0 + i as f32 * 3.5, y), 1.0);
        }
        app.finish_stroke();
        app.settle_strokes();
    };
    let mut raw = app(Some(Color32::WHITE));
    pure_mixing(&mut raw);
    raw.brush_state.brush.brush_options.diameter = 4.0;
    wobbly(&mut raw);
    let mut corrected = app(Some(Color32::WHITE));
    pure_mixing(&mut corrected);
    corrected.brush_state.brush.brush_options.diameter = 4.0;
    corrected.brush_state.brush.stabilizer_algorithm = StabilizerAlgorithm::PostCorrection;
    corrected.brush_state.brush.stabilizer_modes.correction = 1.0;
    // (The smoothing's reach is on screen.)
    corrected.viewport.zoom = 1.0;
    let before = layer(&corrected);
    let pushes = corrected.layer_state.history.push_count();
    wobbly(&mut corrected);
    // (In the middle: the ends stay where they were.)
    let spread = |px: &[Color32]| {
        (0..64)
            .filter(|&y| (45..75).any(|x| painted(px, x, y)))
            .count()
    };
    assert!(
        spread(&layer(&corrected)) < spread(&layer(&raw)),
        "less wobble"
    );
    assert_eq!(corrected.layer_state.history.push_count(), pushes + 1);
    // (It starts where the pen went down, as without it.)
    assert!(painted(&layer(&corrected), 10, 32), "the first dab");
    corrected.apply_history(false);
    assert!(layer(&corrected) == before);
}

#[test]
fn a_mixing_airbrush_keeps_mixing_while_the_pen_rests() {
    let rest = |rate: f32| {
        let mut app = app(Some(Color32::WHITE));
        pure_mixing(&mut app);
        app.brush_state.brush.mixing.as_mut().unwrap().color_rate = 0.3;
        app.brush_state.brush.airbrush_rate = rate;
        app.start_stroke_with_pressure(Vec2::new(60.0, 32.0), 1.0);
        std::thread::sleep(std::time::Duration::from_millis(250));
        app.finish_stroke();
        app.settle_strokes();
        layer(&app)[32 * 128 + 60]
    };
    let (once, resting) = (rest(0.0), rest(80.0));
    assert!(
        resting.r() < once.r(),
        "more of the blue: {once:?} → {resting:?}"
    );
}

#[test]
fn a_parallel_mixing_brush_follows_the_formula() {
    use crate::brush_engine::brush_options::Mixing;
    use crate::canvas::blend_modes::{LayerBlend, composite};
    let grey = Color32::from_rgb(160, 120, 200);
    let mut app = app(Some(grey));
    app.active_tool = crate::app::tools::Tool::Brush;
    // All brush colour, nothing carried: each dab is the colour,
    // blended by Parallel over what's there.
    app.brush_state.brush.mixing = Some(Mixing {
        smudge_length: 0.0,
        color_rate: 1.0,
        ..Default::default()
    });
    app.brush_state.brush.paint_blend = LayerBlend::Parallel;
    let paint = app.brush_state.brush.brush_options.color;
    app.start_stroke_with_pressure(Vec2::new(60.0, 32.0), 1.0);
    app.finish_stroke();
    app.settle_strokes();
    let got = pixel(&app, 60);
    let want = crate::canvas::blend::rgba_to_color32_fast(composite(
        LayerBlend::Parallel,
        crate::canvas::blend::color32_to_linear(paint),
        crate::canvas::blend::color32_to_linear(grey),
        0.0,
    ));
    for (g, w) in got.to_array().iter().zip(want.to_array()) {
        assert!(g.abs_diff(w) <= 2, "{got:?} vs {want:?}");
    }
    assert!(got != grey && got != paint, "a mix of both: {got:?}");
}

/// A brush with an imported colour smudge.
fn krita_brush(app: &mut crate::PainterApp, dulling: bool, rate: f32, colour: f32) {
    use crate::brush_engine::brush_options::{KritaSmudge, Mixing};
    app.active_tool = crate::app::tools::Tool::Brush;
    app.brush_state.brush.mixing = Some(Mixing {
        smudge_length: rate,
        color_rate: colour,
        krita: Some(KritaSmudge {
            dulling,
            smear_alpha: true,
            radius: 0.5,
            ..Default::default()
        }),
        ..Default::default()
    });
}

fn brush_drag(app: &mut crate::PainterApp, from: f32, to: f32) {
    app.start_stroke_with_pressure(Vec2::new(from, 32.0), 1.0);
    let steps = 20;
    for i in 1..=steps {
        let x = from + (to - from) * i as f32 / steps as f32;
        app.add_stroke_point(Vec2::new(x, 32.0), 1.0);
    }
    app.finish_stroke();
    app.settle_strokes();
}

#[test]
fn krita_smudge_on_one_colour_changes_nothing_and_its_first_dab_paints_nothing() {
    let red = Color32::from_rgb(230, 30, 20);
    for dulling in [false, true] {
        let mut a = app(Some(red));
        krita_brush(&mut a, dulling, 1.0, 0.0);
        let before = layer(&a);
        brush_drag(&mut a, 10.0, 110.0);
        assert!(layer(&a) == before, "one colour stays ({dulling})");
        // A tap: the first dab only says where it is.
        let mut a = app(None);
        varied(&mut a);
        krita_brush(&mut a, dulling, 1.0, 1.0);
        let before = layer(&a);
        a.start_stroke_with_pressure(Vec2::new(60.0, 32.0), 1.0);
        a.finish_stroke();
        a.settle_strokes();
        assert!(layer(&a) == before, "a tap paints nothing ({dulling})");
    }
}

#[test]
fn krita_smearing_drags_paint_along_and_undoes_exactly() {
    // Black on the left, white on the right: a stroke out of the black
    // carries it into the white.
    let mut a = app(None);
    for tx in 0..2 {
        let tile = (0..64 * 64)
            .map(|i| {
                if tx * 64 + i % 64 < 40 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                }
            })
            .collect();
        a.canvas_mut().set_layer_tile_data(1, tx, 0, tile);
    }
    krita_brush(&mut a, false, 1.0, 0.0);
    let before = layer(&a);
    brush_drag(&mut a, 20.0, 80.0);
    let p = pixel(&a, 60);
    assert!(p.r() < 200, "dark paint dragged into the white: {p:?}");
    a.apply_history(false);
    assert!(layer(&a) == before, "undo restores");
}

#[test]
fn krita_colour_rate_lays_down_the_brush_colour() {
    let mut a = app(Some(Color32::WHITE));
    krita_brush(&mut a, true, 0.0, 1.0);
    brush_drag(&mut a, 10.0, 110.0);
    // Colour rate 1 at full opacity: rate² × opacity = all brush colour.
    let p = pixel(&a, 60);
    let want = a.brush_state.brush.brush_options.color;
    for (g, w) in p.to_array().iter().zip(want.to_array()) {
        assert!(g.abs_diff(w) <= 2, "{p:?} vs {want:?}");
    }
}

#[test]
fn krita_dulling_mixes_one_colour_where_smearing_keeps_the_pattern() {
    let spread = |dulling: bool| {
        let mut a = app(None);
        for tx in 0..2 {
            let tile = (0..64 * 64)
                .map(|i| {
                    if (i % 64) / 3 % 2 == 0 {
                        Color32::BLACK
                    } else {
                        Color32::WHITE
                    }
                })
                .collect();
            a.canvas_mut().set_layer_tile_data(1, tx, 0, tile);
        }
        krita_brush(&mut a, dulling, 0.5, 0.0);
        brush_drag(&mut a, 10.0, 110.0);
        let row: Vec<i32> = (50..70).map(|x| pixel(&a, x).r() as i32).collect();
        row.iter().max().unwrap() - row.iter().min().unwrap()
    };
    let (dull, smear) = (spread(true), spread(false));
    assert!(dull < smear / 2, "dulling evens out: {dull} vs {smear}");
}

#[test]
fn halton_dulling_lands_near_the_full_weighted_average() {
    let side = 81;
    let codec = Codec::new();
    // Noise under a soft round tip.
    let mut seed = 12345u32;
    let source: Vec<[f32; 4]> = (0..side * side)
        .map(|_| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let v = (seed >> 16) as u8;
            codec.to_f(Color32::from_rgb(v, v / 2, 255 - v))
        })
        .collect();
    let mid = side as f32 * 0.5;
    let mask: Vec<f32> = (0..side * side)
        .map(|i| {
            let (x, y) = ((i % side) as f32 + 0.5 - mid, (i / side) as f32 + 0.5 - mid);
            (1.0 - (x * x + y * y).sqrt() / 40.0).max(0.0)
        })
        .collect();
    let (mut sum, mut weight) = ([0.0f32; 4], 0.0f32);
    for (px, &w) in source.iter().zip(&mask) {
        for k in 0..4 {
            sum[k] += px[k] * w;
        }
        weight += w;
    }
    let full = sum.map(|v| v / weight);
    let (quick, enough) = halton_dull(&source, Some(&mask), side, 40.0);
    assert!(enough, "paint");
    // Pure noise is the worst case: it stops once a batch moves the
    // colour by 2/255 or less (as Krita does), a few levels off.
    for k in 0..4 {
        assert!(
            (quick[k] - full[k]).abs() * 255.0 <= 10.0,
            "{quick:?} vs {full:?}"
        );
    }
    // Nothing under the tip: not enough, and transparent.
    let (none, enough) = halton_dull(&source, Some(&vec![0.0; side * side]), side, 40.0);
    assert!(!enough && none == [0.0; 4]);
}

#[test]
fn no_colour_rate_is_the_plain_smudge() {
    let red = Color32::from_rgb(230, 30, 20);
    let mut plain = app(Some(red));
    drag(&mut plain);
    let mut zero = app(Some(red));
    zero.workspace.blend.color_rate = 0.0;
    drag(&mut zero);
    for x in 0..128 {
        assert_eq!(pixel(&plain, x), pixel(&zero, x));
    }
    assert_eq!(
        pixel(&plain, 60),
        red,
        "smudging one colour changes nothing"
    );
}

/// Layer 1 black left of `edge`, white from it on.
fn black_then_white(app: &mut crate::PainterApp, edge: i32) {
    for tx in 0..2 {
        let tile = (0..64 * 64)
            .map(|i| {
                if tx * 64 + i % 64 < edge {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                }
            })
            .collect();
        app.canvas_mut().set_layer_tile_data(1, tx, 0, tile);
    }
}

#[test]
fn a_faint_smudge_builds_up_in_8_bits_as_in_16() {
    use crate::canvas::storage::Depth;
    // Dragged out of the black so faintly each dab darkens the white by
    // less than half an 8-bit step: the stroke keeps what it mixed at full
    // precision, so the dabs add up even in 8 bits.
    let smudged = |depth: Depth| {
        let mut a = app(None);
        black_then_white(&mut a, 60);
        a.convert_depth(depth);
        a.brush_state.brush.brush_options.opacity = 0.002;
        drag(&mut a);
        a
    };
    let eight = smudged(Depth::U8);
    let mut sixteen = smudged(Depth::U16);
    let at = 32 * 64 + 62;
    let deep = sixteen.canvas.get_layer_tile_deep(1, 0, 0).unwrap();
    let lin = deep.linear(at)[0];
    assert!(lin < 0.999, "the faint dabs build up: {lin}");
    assert_eq!(
        sixteen.canvas.get_layer_tile_data(1, 0, 0).unwrap(),
        deep.narrow_all(),
        "the 8-bit pixels are the deep ones rounded"
    );
    let narrowed = deep.narrow_all();
    for x in 0..64 {
        let (got, want) = (pixel(&eight, x), narrowed[(32 * 64 + x) as usize]);
        for (g, w) in got.to_array().iter().zip(want.to_array()) {
            assert!(g.abs_diff(w) <= 1, "x {x}: {got:?} vs {want:?}");
        }
    }
    // Undone: white at full depth again.
    sixteen.apply_history(false);
    let deep = sixteen.canvas.get_layer_tile_deep(1, 0, 0).unwrap();
    assert_eq!(deep.linear(at), [1.0; 4]);
}

#[test]
fn slow_moves_add_up_to_whole_pixel_moves() {
    // A hard edge under the middle of an imported smear at full rate: each
    // dab copies the layer from where the last one was, by whole pixels (as
    // Krita does: never resampled, so the edge stays sharp), the moves
    // adding up as the dabs cross pixels.
    let mut a = app(None);
    black_then_white(&mut a, 60);
    krita_brush(&mut a, false, 1.0, 0.0);
    let o = &mut a.brush_state.brush.brush_options;
    o.auto_spacing = None;
    o.spacing = 2.5;
    // How much black row 32 holds round the edge (in pixels' worth).
    let black = |a: &crate::PainterApp| -> f32 {
        let codec = Codec::new();
        (50..70).map(|x| 1.0 - codec.to_f(pixel(a, x))[0]).sum()
    };
    let before = black(&a);
    // Half a pixel a dab, one pixel in all.
    a.start_stroke_with_pressure(Vec2::new(60.0, 32.0), 1.0);
    a.add_stroke_point(Vec2::new(60.5, 32.0), 1.0);
    a.add_stroke_point(Vec2::new(61.0, 32.0), 1.0);
    a.finish_stroke();
    a.settle_strokes();
    let moved = black(&a) - before;
    let edge: Vec<u8> = (58..64).map(|x| pixel(&a, x).r()).collect();
    assert!(
        (moved - 1.0).abs() < 0.01,
        "the edge moved as far as the brush: {moved} {edge:?}"
    );
    assert!(
        edge.iter().all(|&r| r == 0 || r == 255),
        "on whole pixels: {edge:?}"
    );
}

#[test]
fn the_smudge_length_counts_under_a_full_strength_tip() {
    // Black dragged into white by a hard tip at full strength: a short
    // length lets it go sooner. (Picking up what it had just laid, the
    // brush kept all it carried wherever the tip was at full strength,
    // whatever the length.)
    let far = |length: f32| {
        let mut a = app(None);
        black_then_white(&mut a, 20);
        a.workspace.blend.smudge_length = length;
        drag(&mut a);
        pixel(&a, 80).r()
    };
    let (short, long) = (far(0.3), far(0.8));
    assert!(short > 200 && short > long + 50, "{short} vs {long}");
}

#[test]
fn a_blur_dab_takes_in_what_lies_just_past_it() {
    // White starts just past a hard tip's edge: the blur near the edge
    // takes it in, as filtering the layer would; deeper in, only black.
    let mut a = app(None);
    black_then_white(&mut a, 51);
    a.active_tool = crate::app::tools::Tool::Blur;
    a.blend_press(Vec2::new(40.0, 32.0), 1.0);
    a.blend_release();
    a.settle_strokes();
    assert!(pixel(&a, 49).r() > 0, "{:?}", pixel(&a, 49));
    assert_eq!(pixel(&a, 40), Color32::BLACK);
}

#[test]
fn a_gamma_space_document_blurs_in_gamma() {
    // Black and white stripes blurred together: half way between in the
    // stored sRGB values in gamma space, much lighter mixed as light (at
    // full depth too).
    use crate::canvas::storage::Depth;
    let grey = |space: crate::canvas::blend_modes::BlendSpace, depth: Depth| {
        let mut a = app(None);
        for tx in 0..2 {
            let tile = (0..64 * 64)
                .map(|i| {
                    if i % 2 == 0 {
                        Color32::BLACK
                    } else {
                        Color32::WHITE
                    }
                })
                .collect();
            a.canvas_mut().set_layer_tile_data(1, tx, 0, tile);
        }
        a.canvas_mut().blend_space = space;
        a.convert_depth(depth);
        a.active_tool = crate::app::tools::Tool::Blur;
        a.workspace.blend.blur_size = 0.5;
        a.blend_press(Vec2::new(60.0, 32.0), 1.0);
        a.blend_release();
        a.settle_strokes();
        pixel(&a, 60).r()
    };
    use crate::canvas::blend_modes::BlendSpace;
    for depth in [Depth::U8, Depth::U16] {
        let (gamma, linear) = (
            grey(BlendSpace::Gamma, depth),
            grey(BlendSpace::Linear, depth),
        );
        assert!((110..=150).contains(&gamma), "{depth:?}: gamma {gamma}");
        assert!(linear >= 170, "{depth:?}: linear {linear}");
    }
}

#[test]
fn krita_smudge_takes_each_dab_s_colour() {
    // Hue randomness: the colour mixed in turns from the brush's.
    let mut a = app(Some(Color32::WHITE));
    krita_brush(&mut a, false, 0.5, 1.0);
    a.brush_state.brush.dynamics.random.hue = 120.0;
    brush_drag(&mut a, 10.0, 110.0);
    let px = layer(&a);
    assert!(
        px.iter().any(|c| c.r() > 60 || c.g() > 80),
        "some dab isn't the brush's blue"
    );
}

/// Two dabs of the Brush tool on row 32, at 60 and 70.
fn two_dabs(a: &mut crate::PainterApp) {
    let o = &mut a.brush_state.brush.brush_options;
    o.auto_spacing = None;
    o.spacing = 50.0;
    a.start_stroke_with_pressure(Vec2::new(60.0, 32.0), 1.0);
    a.add_stroke_point(Vec2::new(70.0, 32.0), 1.0);
    a.finish_stroke();
    a.settle_strokes();
}

/// White mixed with `share` of the brush colour (as light).
fn white_with(a: &crate::PainterApp, share: f32) -> Color32 {
    let codec = Codec::new();
    let paint = codec.to_f(a.brush_state.brush.brush_options.color);
    codec.to_c(std::array::from_fn(|c| 1.0 + (paint[c] - 1.0) * share))
}

fn close(got: Color32, want: Color32) -> bool {
    (got.to_array().iter().zip(want.to_array())).all(|(g, w)| g.abs_diff(w) <= 2)
}

#[test]
fn krita_s_older_engine_puts_its_mix_down_at_the_smudge_rate() {
    // Dulling on white at smudge rate 0.5 and colour rate 1: the older
    // engine mixes in half the colour (what the most smudge leaves) and
    // puts that down at the smudge rate, a quarter in all; the new one
    // lays all the colour (the colour rate squared, put down whole).
    let laid = |legacy: bool| {
        let mut a = app(Some(Color32::WHITE));
        krita_brush(&mut a, true, 0.5, 1.0);
        if let Some(k) = a
            .brush_state
            .brush
            .mixing
            .as_mut()
            .and_then(|m| m.krita.as_mut())
        {
            k.legacy = legacy;
        }
        two_dabs(&mut a);
        (
            pixel(&a, 70),
            white_with(&a, if legacy { 0.25 } else { 1.0 }),
        )
    };
    for legacy in [true, false] {
        let (got, want) = laid(legacy);
        assert!(close(got, want), "{legacy}: {got:?} vs {want:?}");
    }
}

#[test]
fn krita_dulling_over_the_layer_mixes_the_colour_in_first() {
    // Without smear alpha, Krita mixes the brush colour into the dulled
    // colour and lays that at the dulling rate (0.8 × the smudge rate):
    // eight tenths of the brush colour, not all of it.
    let mut a = app(Some(Color32::WHITE));
    krita_brush(&mut a, true, 1.0, 1.0);
    if let Some(k) = a
        .brush_state
        .brush
        .mixing
        .as_mut()
        .and_then(|m| m.krita.as_mut())
    {
        k.smear_alpha = false;
    }
    two_dabs(&mut a);
    let (got, want) = (pixel(&a, 70), white_with(&a, 0.8));
    assert!(close(got, want), "{got:?} vs {want:?}");
}

#[test]
fn krita_s_older_dulling_samples_past_the_dab() {
    // White round the dabs, black farther out: sampling the dab's own
    // square stays white; three times it takes in the black.
    let dulled = |radius: f32| {
        let mut a = app(None);
        for tx in 0..2 {
            let tile = (0..64 * 64)
                .map(|i| {
                    if (45..86).contains(&(tx * 64 + i % 64)) {
                        Color32::WHITE
                    } else {
                        Color32::BLACK
                    }
                })
                .collect();
            a.canvas_mut().set_layer_tile_data(1, tx, 0, tile);
        }
        krita_brush(&mut a, true, 1.0, 0.0);
        if let Some(k) = a
            .brush_state
            .brush
            .mixing
            .as_mut()
            .and_then(|m| m.krita.as_mut())
        {
            k.legacy = true;
            k.radius = radius;
        }
        two_dabs(&mut a);
        pixel(&a, 70).r()
    };
    let (own, wide) = (dulled(1.0), dulled(3.0));
    // (A 63 px square round 60: 22 of its columns black.)
    assert!(own == 255 && (200..225).contains(&wide), "{own} vs {wide}");
}
