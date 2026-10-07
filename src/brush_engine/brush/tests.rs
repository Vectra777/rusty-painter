use super::*;
use rayon::ThreadPoolBuilder;

#[test]
fn pressure_can_drive_opacity_and_restores_the_brush() {
    use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
    use crate::canvas::history::UndoAction;
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let alpha_at = |pressure: f32| {
        let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        canvas.active_layer_idx = 1;
        let mut brush = Brush::new(12.0, 100.0, Color32::BLACK, 10.0);
        brush.brush_options.pressure_size = false;
        brush.brush_options.pressure_opacity = true;
        let mut undo = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let mut tiles = StrokeTiles::default();
        let mut stroke = StrokeState::new();
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        stroke.add_point(&mut brush, Vec2::new(32.0, 32.0), pressure, &mut ctx);
        assert_eq!(
            brush.brush_options.opacity, 1.0,
            "opacity restored after the sample"
        );
        assert_eq!(brush.brush_options.diameter, 12.0, "diameter restored");
        canvas.get_layer_tile_data(1, 0, 0).unwrap()[32 * 64 + 32].a()
    };
    let (light, full) = (alpha_at(0.25), alpha_at(1.0));
    assert!(
        light < full,
        "light pressure {light} should paint lighter than full {full}"
    );
    assert!(
        (60..=68).contains(&light),
        "about a quarter opacity, got {light}"
    );
}

#[test]
fn gamma_documents_mix_strokes_as_stored_values() {
    use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
    use crate::canvas::blend_modes::BlendSpace;
    use crate::canvas::history::UndoAction;
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let stroke_on = |space: BlendSpace| {
        // Paint on the (white) background layer itself so the stroke mixes
        // with white.
        let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        canvas.blend_space = space;
        canvas.active_layer_idx = 0;
        let mut brush = Brush::new(12.0, 100.0, Color32::BLACK, 10.0);
        brush.brush_options.opacity = 0.5;
        let mut undo = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let mut tiles = StrokeTiles::default();
        let mut stroke = StrokeState::new();
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        stroke.add_point(&mut brush, Vec2::new(32.0, 32.0), 1.0, &mut ctx);
        canvas.get_layer_tile_data(0, 0, 0).unwrap()[32 * 64 + 32].r()
    };
    assert_eq!(stroke_on(BlendSpace::Linear), 188);
    assert_eq!(stroke_on(BlendSpace::Gamma), 128);
}

#[test]
fn stroke_leaving_the_canvas_paints_only_inside() {
    use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
    use crate::canvas::history::UndoAction;
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let mut canvas = Canvas::new(256, 256, Color32::WHITE, 64);
    canvas.active_layer_idx = 1;
    let mut brush = Brush::new(10.0, 100.0, Color32::BLACK, 10.0);
    let mut undo = UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    };
    let mut tiles = StrokeTiles::default();
    let mut stroke = StrokeState::new();
    let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
    // Up through the top edge at x=64, along outside, back in at x=192.
    for p in [
        (64.0, 128.0),
        (64.0, -100.0),
        (192.0, -100.0),
        (192.0, 128.0),
    ] {
        stroke.add_point(&mut brush, Vec2::new(p.0, p.1), 1.0, &mut ctx);
    }
    let painted = |x: i32, y: i32| {
        let data = canvas
            .get_layer_tile_data(1, x / 64, y / 64)
            .unwrap_or_default();
        data.get(((y % 64) * 64 + x % 64) as usize)
            .is_some_and(|p| p.a() > 0)
    };
    assert!(
        painted(64, 2) && painted(192, 2),
        "both crossings reach the edge"
    );
    for x in 80..176 {
        assert!(
            !painted(x, 0),
            "top edge at x={x} painted: stroke was clamped"
        );
    }
}

#[test]
fn chord_never_cuts_off_covered_pixels() {
    // Full-row kernel vs chord-restricted kernel, over many radii,
    // sub-pixel offsets and rows: every non-zero pixel must be inside the
    // chord, and the values inside must be identical.
    for &r in &[0.6_f32, 1.0, 2.3, 7.5, 31.0, 120.25] {
        let tip = GaussianTip::new(r, 0.4);
        let len = (2 * tip.r_ceil + 3) as usize;
        for step in 0..7 {
            let frac = step as f32 / 7.0;
            for my in 0..len {
                let pdy = my as f32 - tip.r_ceil as f32 + 0.5 - frac;
                for mx0 in [0usize, 1, 3] {
                    let n = len.saturating_sub(mx0);
                    let mut full = vec![0.0; n];
                    tip.row(pdy, frac, mx0, &mut full);
                    let chord = tip.chord(pdy, frac, mx0, n);
                    let mut part = vec![0.0; n];
                    if !chord.is_empty() {
                        tip.row(pdy, frac, mx0 + chord.start, &mut part[chord.clone()]);
                    }
                    assert_eq!(full, part, "r {r}, frac {frac}, row {my}, mx0 {mx0}");
                }
            }
        }
    }
}
use std::collections::HashSet;

/// Deterministic, dependency-free hash (FNV-1a) over raw RGBA bytes.
/// Used as a golden-master checksum: captured once from known-correct
/// output, then asserted unchanged across refactors of the pixel-stamp
/// hot path, which is otherwise very hard to regression-test without a
/// display to visually compare rendered frames.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

const TILE_SIZE_FOR_TEST: usize = 64;

/// Paint each `(brush, centers)` step onto a shared fresh 2x2-tile
/// canvas in order, then return an FNV-1a checksum of every tile's raw
/// pixel bytes (layer 1, the paintable default layer). Spans multiple
/// tiles so both the single-tile and multi-tile/parallel dispatch code
/// paths run. Multiple steps let a scenario paint a base fill, then
/// erase/blend on top of it within the same checksum.
fn paint_and_checksum(steps: &[(Brush, Vec<Vec2>)]) -> u64 {
    paint_and_checksum_in(steps, None)
}

/// A concave lasso covering part of the stroke, for selection goldens.
fn star_selection() -> SelectionManager {
    let star: Vec<Vec2> = (0..17)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 17.0;
            let r = if i % 2 == 0 { 38.0 } else { 17.3 };
            Vec2::new(50.2 + a.cos() * r, 46.7 + a.sin() * r)
        })
        .collect();
    SelectionManager::with_shape(Some(crate::selection::new_lasso_shape(star)))
}

fn paint_and_checksum_in(
    steps: &[(Brush, Vec<Vec2>)],
    selection: Option<&SelectionManager>,
) -> u64 {
    fnv1a(&paint_bytes_in(steps, selection))
}

fn paint_bytes_in(steps: &[(Brush, Vec<Vec2>)], selection: Option<&SelectionManager>) -> Vec<u8> {
    let canvas = Canvas::new(96, 96, Color32::TRANSPARENT, TILE_SIZE_FOR_TEST);
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();

    for (brush, centers) in steps {
        let brush = brush.clone();
        let mut undo_action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let mut stroke_tiles = StrokeTiles::default();
        // One batch per step: the goldens were captured painting one dab
        // at a time, so matching them proves batching changes nothing.
        brush.dabs(
            &pool,
            &canvas,
            selection,
            centers,
            &mut undo_action,
            &mut stroke_tiles,
        );
    }

    let mut bytes = Vec::new();
    for ty in 0..2 {
        for tx in 0..2 {
            let data = canvas.get_layer_tile_data(1, tx, ty).unwrap_or_else(|| {
                vec![Color32::TRANSPARENT; TILE_SIZE_FOR_TEST * TILE_SIZE_FOR_TEST]
            });
            for pixel in data {
                bytes.extend_from_slice(&pixel.to_array());
            }
        }
    }
    bytes
}

fn stroke_centers() -> Vec<Vec2> {
    // A short diagonal stroke crossing all four tiles of the 2x2 grid.
    vec![
        Vec2::new(20.0, 20.0),
        Vec2::new(40.0, 40.0),
        Vec2::new(60.0, 60.0),
        Vec2::new(80.0, 80.0),
    ]
}

/// Golden-master check for the hard-edged Pixel brush path
/// (`Brush::pixel_dab`). If this fails after a refactor, the refactor
/// changed pixel output, not just structure.
#[test]
fn pixel_dab_output_is_stable() {
    let mut brush = Brush::new_pixel(18.0, Color32::from_rgba_unmultiplied(200, 30, 30, 255));
    brush.brush_options.pixel_shape = PixelBrushShape::Circle;
    let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
    assert_eq!(
        checksum, 0xc540cd961ef28f05,
        "GOLDEN_PLACEHOLDER:pixel_dab_output_is_stable"
    );
}

/// Golden-master check for the Pixel brush with a Square tip.
#[test]
fn pixel_dab_square_output_is_stable() {
    let mut brush = Brush::new_pixel(18.0, Color32::from_rgba_unmultiplied(30, 200, 30, 255));
    brush.brush_options.pixel_shape = PixelBrushShape::Square;
    let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
    assert_eq!(
        checksum, 0x30fed29cce6020a5,
        "GOLDEN_PLACEHOLDER:pixel_dab_square_output_is_stable"
    );
}

#[test]
fn dirty_tiles_track_only_dabs_since_last_drain() {
    let canvas = Canvas::new(96, 96, Color32::TRANSPARENT, TILE_SIZE_FOR_TEST);
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let brush = Brush::new(8.0, 50.0, Color32::BLACK, 25.0);
    let mut undo_action = UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    };
    let mut stroke_tiles = StrokeTiles::default();

    let first = Vec2::new(16.0, 16.0);
    brush.dabs(
        &pool,
        &canvas,
        None,
        &[first],
        &mut undo_action,
        &mut stroke_tiles,
    );
    stroke_tiles.dirty.clear();

    let second = Vec2::new(80.0, 80.0);
    brush.dabs(
        &pool,
        &canvas,
        None,
        &[second],
        &mut undo_action,
        &mut stroke_tiles,
    );

    assert_eq!(stroke_tiles.dirty, HashSet::from([(1, 1)]));
    let buffered: HashSet<_> = stroke_tiles.buffers.keys().copied().collect();
    assert_eq!(buffered, HashSet::from([(0, 0), (1, 1)]));
}

/// Golden-master check for the Soft brush's fast Gaussian-circle path
/// (anti_aliasing + Gaussian + Circle + Normal blend + no selection).
#[test]
fn soft_dab_gaussian_fast_path_output_is_stable() {
    let brush = Brush::new(
        24.0,
        40.0,
        Color32::from_rgba_unmultiplied(30, 30, 200, 255),
        15.0,
    );
    assert!(brush.anti_aliasing);
    assert_eq!(
        brush.brush_options.softness_selector,
        SoftnessSelector::Gaussian
    );
    assert_eq!(brush.brush_options.pixel_shape, PixelBrushShape::Circle);
    let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
    assert_eq!(
        checksum, 0x2e19e339e81f9a29,
        "GOLDEN_PLACEHOLDER:soft_dab_gaussian_fast_path_output_is_stable"
    );
}

/// Golden-master check for the Soft brush's general (non-fast-path)
/// path, forced by a Square tip.
#[test]
fn soft_dab_general_path_square_output_is_stable() {
    let mut brush = Brush::new(
        24.0,
        40.0,
        Color32::from_rgba_unmultiplied(200, 200, 30, 255),
        15.0,
    );
    brush.brush_options.pixel_shape = PixelBrushShape::Square;
    let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
    assert_eq!(
        checksum, 0x73e7076fcdc50db1,
        "GOLDEN_PLACEHOLDER:soft_dab_general_path_square_output_is_stable"
    );
}

/// Golden-master check for the Soft brush's general path without
/// anti-aliasing (hard edges, the falloff kept inside, as in Krita).
#[test]
fn soft_dab_general_path_no_aa_output_is_stable() {
    let mut brush = Brush::new(
        24.0,
        40.0,
        Color32::from_rgba_unmultiplied(200, 30, 200, 255),
        15.0,
    );
    brush.anti_aliasing = false;
    let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
    assert_eq!(
        checksum, 0x3add87818d40766a,
        "GOLDEN_PLACEHOLDER:soft_dab_general_path_no_aa_output_is_stable"
    );
}

/// Golden-master check for the Eraser blend mode, erasing into a
/// pre-painted solid fill.
#[test]
fn soft_dab_eraser_output_is_stable() {
    let fill_brush = Brush::new(
        60.0,
        0.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, 255),
        15.0,
    );
    let mut eraser_brush = Brush::new(
        24.0,
        40.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, 255),
        15.0,
    );
    eraser_brush.brush_options.blend_mode = BlendMode::Eraser;

    let checksum = paint_and_checksum(&[
        (fill_brush, vec![Vec2::new(48.0, 48.0)]),
        (eraser_brush, stroke_centers()),
    ]);
    assert_eq!(
        checksum, 0x26f16f7573c9fa96,
        "GOLDEN_PLACEHOLDER:soft_dab_eraser_output_is_stable"
    );
}

#[test]
fn soft_dab_selection_output_is_stable() {
    let brush = Brush::new(
        24.0,
        40.0,
        Color32::from_rgba_unmultiplied(30, 150, 90, 255),
        15.0,
    );
    let checksum = paint_and_checksum_in(&[(brush, stroke_centers())], Some(&star_selection()));
    assert_eq!(
        checksum, 0x58c2a1b16f52a5a8,
        "GOLDEN_PLACEHOLDER:soft_dab_selection_output_is_stable"
    );
}

#[test]
fn selection_only_changes_pixels_on_its_antialiased_edge() {
    let brush = Brush::new(
        24.0,
        40.0,
        Color32::from_rgba_unmultiplied(30, 150, 90, 255),
        15.0,
    );
    let steps = [(brush, stroke_centers())];
    let free = paint_bytes_in(&steps, None);
    let selection = star_selection();
    let masked = paint_bytes_in(&steps, Some(&selection));

    // paint_bytes_in lays tiles out (0,0), (1,0), (0,1), (1,1), 64x64 each.
    let mut checked_inside = 0;
    for tile in 0..4 {
        let (tx, ty) = (tile % 2, tile / 2);
        for ly in 0..64 {
            let mut row = [0.0f32; 64];
            selection.row_coverage(ty * 64 + ly, tx * 64, &mut row);
            for (lx, &cov) in row.iter().enumerate() {
                let i = (tile * 64 * 64 + ly * 64 + lx) * 4;
                if cov == 1.0 {
                    assert_eq!(masked[i..i + 4], free[i..i + 4]);
                    checked_inside += 1;
                } else if cov == 0.0 {
                    assert_eq!(masked[i..i + 4], [0, 0, 0, 0]);
                }
            }
        }
    }
    assert!(checked_inside > 500);
}

#[test]
fn pixel_dab_selection_output_is_stable() {
    let brush = Brush::new_pixel(18.0, Color32::from_rgba_unmultiplied(90, 30, 150, 255));
    let checksum = paint_and_checksum_in(&[(brush, stroke_centers())], Some(&star_selection()));
    assert_eq!(
        checksum, 0x322bba2014492c15,
        "GOLDEN_PLACEHOLDER:pixel_dab_selection_output_is_stable"
    );
}

/// Dab centers off the pixel grid, so sub-pixel placement and rounding
/// both matter.
fn fractional_centers() -> Vec<Vec2> {
    (0..30)
        .map(|i| Vec2::new(14.3 + i as f32 * 2.37, 20.7 + (i as f32 * 0.4).sin() * 25.1))
        .collect()
}

#[test]
fn soft_partial_flow_fractional_output_is_stable() {
    let mut brush = Brush::new(
        30.0,
        30.0,
        Color32::from_rgba_unmultiplied(220, 120, 40, 255),
        10.0,
    );
    brush.brush_options.flow = 50.0;
    let checksum = paint_and_checksum(&[(brush, fractional_centers())]);
    assert_eq!(
        checksum, 0x6c7f6feffff7bc06,
        "GOLDEN_PLACEHOLDER:soft_partial_flow_fractional_output_is_stable"
    );
}

#[test]
fn eraser_partial_flow_fractional_output_is_stable() {
    let fill = Brush::new(
        60.0,
        0.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, 255),
        15.0,
    );
    let mut eraser = Brush::new(24.0, 40.0, Color32::BLACK, 15.0);
    eraser.brush_options.blend_mode = BlendMode::Eraser;
    eraser.brush_options.flow = 40.0;
    let checksum = paint_and_checksum(&[
        (fill, vec![Vec2::new(48.0, 48.0)]),
        (eraser, fractional_centers()),
    ]);
    assert_eq!(
        checksum, 0xc844f31153ed347a,
        "GOLDEN_PLACEHOLDER:eraser_partial_flow_fractional_output_is_stable"
    );
}

fn max_alpha(bytes: &[u8]) -> u8 {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| px[3])
        .max()
        .unwrap_or(0)
}

#[test]
fn wash_mode_caps_a_stroke_at_its_opacity() {
    let dense: Vec<Vec2> = (0..40)
        .map(|i| Vec2::new(40.0 + i as f32 * 0.5, 48.0))
        .collect();
    let mut brush = Brush::new(30.0, 80.0, Color32::from_rgb(20, 90, 200), 5.0);
    brush.brush_options.opacity = 0.5;
    brush.brush_options.flow = 60.0;

    let build_up = max_alpha(&paint_bytes_in(&[(brush.clone(), dense.clone())], None));
    brush.brush_options.painting_mode = PaintingMode::Wash;
    let wash = max_alpha(&paint_bytes_in(&[(brush, dense)], None));

    assert!(
        build_up > 250,
        "build-up keeps accumulating past opacity: {build_up}"
    );
    assert!(
        (127..=128).contains(&wash),
        "wash tops out at opacity: {wash}"
    );
}

#[test]
fn pixel_brush_honors_opacity_and_flow() {
    let mut brush = Brush::new_pixel(9.0, Color32::from_rgb(200, 40, 40));
    brush.brush_options.opacity = 0.5;
    let half = max_alpha(&paint_bytes_in(
        &[(brush.clone(), vec![Vec2::new(20.5, 20.5)])],
        None,
    ));
    brush.brush_options.opacity = 1.0;
    brush.brush_options.flow = 25.0;
    let quarter = max_alpha(&paint_bytes_in(
        &[(brush, vec![Vec2::new(20.5, 20.5)])],
        None,
    ));
    assert!((127..=128).contains(&half), "{half}");
    assert!((63..=64).contains(&quarter), "{quarter}");
}

#[test]
fn gaussian_tip_matches_soft_brush_formula() {
    let r = 12.0;
    let hardness = 0.2;
    let tip = GaussianTip::new(r, hardness);
    let (frac_x, frac_y) = (0.25, 0.5);
    let my = 12usize;
    let pdy = my as f32 - 12.0 + 0.5 - frac_y;
    let mut row = [0.0f32; 25];
    tip.row(pdy, frac_x, 0, &mut row);

    for (mx, &alpha) in row.iter().enumerate() {
        let pdx = mx as f32 - 12.0 + 0.5 - frac_x;
        let soft = |t| super::super::masks::gaussian_falloff(t, hardness);
        let expected = super::super::masks::auto_tip_alpha(
            (pdx, pdy),
            r,
            false,
            SoftnessSelector::Gaussian,
            soft,
            true,
        );
        assert!((alpha - expected).abs() <= 1e-5, "mx={mx}");
    }
}

#[test]
fn gaussian_tip_kernels_match_scalar_reference() {
    for hardness in [0.0, 0.2, 0.5, 0.99, 1.0] {
        for r in [0.6_f32, 3.0, 12.0, 40.5] {
            let tip = GaussianTip::new(r, hardness);
            let side = (2 * tip.r_ceil + 1) as usize;
            for (frac_x, frac_y) in [(0.0, 0.0), (0.3125, 0.9375), (0.5, 0.0625)] {
                for my in 0..side {
                    let pdy = my as f32 - tip.r_ceil as f32 + 0.5 - frac_y;
                    // Odd starts and lengths exercise vector bodies and tails.
                    let (mx0, len) = (my % 3, side - my % 3);
                    let mut baseline = vec![0.0f32; len];
                    let mut dispatched = vec![0.0f32; len];
                    row_kernel(&tip, pdy, frac_x, mx0, &mut baseline);
                    tip.row(pdy, frac_x, mx0, &mut dispatched);
                    for i in 0..len {
                        let pdx = (mx0 + i) as f32 - tip.r_ceil as f32 + 0.5 - frac_x;
                        let dist_sq = pdx * pdx + pdy * pdy;
                        let dist = dist_sq.sqrt();
                        let expected = tip.alpha(dist_sq, dist, dist * tip.inv_radius).to_bits();
                        assert_eq!(
                            baseline[i].to_bits(),
                            expected,
                            "h={hardness} r={r} my={my} i={i}"
                        );
                        assert_eq!(
                            dispatched[i].to_bits(),
                            expected,
                            "h={hardness} r={r} my={my} i={i}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn batched_dabs_match_one_at_a_time() {
    let centers: Vec<Vec2> = (0..40)
        .map(|i| Vec2::new(30.0 + i as f32 * 1.7, 34.0 + (i as f32 * 0.9).sin() * 20.0))
        .collect();
    let brushes = [
        Brush::new(
            40.0,
            30.0,
            Color32::from_rgba_unmultiplied(20, 90, 200, 180),
            10.0,
        ),
        Brush::new_pixel(9.0, Color32::from_rgba_unmultiplied(200, 40, 40, 255)),
    ];
    for brush in brushes {
        let paint = |batched: bool| {
            let canvas = Canvas::new(128, 128, Color32::TRANSPARENT, 32);
            let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
            let brush = brush.clone();
            let mut undo = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut tiles = StrokeTiles::default();
            if batched {
                brush.dabs(&pool, &canvas, None, &centers, &mut undo, &mut tiles);
            } else {
                for c in &centers {
                    brush.dabs(&pool, &canvas, None, &[*c], &mut undo, &mut tiles);
                }
            }
            (0..4)
                .flat_map(|ty| (0..4).map(move |tx| (tx, ty)))
                .map(|(tx, ty)| canvas.get_layer_tile_data(1, tx, ty))
                .collect::<Vec<_>>()
        };
        assert!(paint(true) == paint(false));
    }
}

#[test]
fn a_tile_painted_in_bands_matches_it_painted_whole() {
    // Big dabs close together: a batch puts far more than a thread's
    // share on each tile, so its rows are painted in bands; one dab at
    // a time never does.
    let centers: Vec<Vec2> = (0..30)
        .map(|i| Vec2::new(60.0 + i as f32 * 2.0, 64.0 + (i as f32 * 0.4).sin() * 8.0))
        .collect();
    let mut picture = Brush::new(
        110.0,
        50.0,
        Color32::from_rgba_unmultiplied(200, 120, 30, 160),
        2.0,
    );
    picture.brush_options.pixel_shape = PixelBrushShape::Custom(std::sync::Arc::clone(
        &crate::brush_engine::tip::builtin()[1].1,
    ));
    let brushes = [
        Brush::new(
            110.0,
            30.0,
            Color32::from_rgba_unmultiplied(20, 90, 200, 180),
            2.0,
        ),
        picture,
    ];
    for brush in brushes {
        let paint = |batched: bool| {
            let canvas = Canvas::new(192, 128, Color32::TRANSPARENT, 64);
            let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
            let mut undo = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut tiles = StrokeTiles::default();
            if batched {
                brush.dabs(&pool, &canvas, None, &centers, &mut undo, &mut tiles);
            } else {
                for c in &centers {
                    brush.dabs(&pool, &canvas, None, &[*c], &mut undo, &mut tiles);
                }
            }
            (0..2)
                .flat_map(|ty| (0..3).map(move |tx| (tx, ty)))
                .map(|(tx, ty)| canvas.get_layer_tile_data(1, tx, ty))
                .collect::<Vec<_>>()
        };
        let whole = paint(true);
        assert!(whole.iter().any(|t| t.is_some()), "it painted");
        assert!(whole == paint(false));
    }
}
