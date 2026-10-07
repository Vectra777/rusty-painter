//! Strokes in 16-bit and 32-bit float documents: paint too faint for 8 bits
//! still builds up, and undo puts the pixels back at full depth.

use super::*;
use crate::canvas::storage::{DeepTile, Depth};

/// A canvas at `depth` whose paint layer is solid white.
fn white_at(depth: Depth) -> Canvas {
    let mut canvas = painted(Color32::WHITE);
    canvas.convert_depth(depth);
    canvas
}

/// Black so faint one stroke moves white by less than half an 8-bit step.
fn faint_black() -> Brush {
    let mut b = brush(BrushDynamics::default());
    b.brush_options.opacity = 0.00002;
    b
}

/// Tile (0, 1)'s pixels at full depth: under the stroke along y = 64.
fn deep_tile(canvas: &Canvas) -> DeepTile {
    canvas.get_layer_tile_deep(1, 0, 1).expect("a deep tile")
}

/// Under the middle of the stroke along y = 70: x = 32, row 6 of tile (0, 1).
const MIDDLE: usize = 6 * 64 + 32;

#[test]
fn faint_strokes_build_up_in_deep_documents_and_not_in_8_bits() {
    let eight = painted(Color32::WHITE);
    let canvases = [Depth::U16, Depth::F32].map(white_at);
    let mut b = faint_black();
    for pass in 0..50 {
        paint_on(&eight, &mut b, &line(70.0, 0.5), pass);
        for canvas in &canvases {
            paint_on(canvas, &mut b, &line(70.0, 0.5), pass);
        }
    }
    let eight_px = eight.get_layer_tile_data(1, 0, 1).unwrap()[MIDDLE];
    assert_eq!(
        eight_px,
        Color32::WHITE,
        "each stroke rounds away in 8 bits"
    );
    for canvas in &canvases {
        let deep = deep_tile(canvas);
        let lin = deep.linear(MIDDLE);
        assert!(
            lin[0] < 0.99 && lin[0] > 0.97,
            "{:?}: fifty faint strokes darken white ({})",
            canvas.depth(),
            lin[0]
        );
        assert_eq!(lin[3], 1.0);
        assert!(deep.narrow(MIDDLE).r() < 255);
        // The 8-bit pixels are the deep ones rounded.
        assert_eq!(
            canvas.get_layer_tile_data(1, 0, 1).unwrap(),
            deep.narrow_all(),
            "{:?}",
            canvas.depth()
        );
    }
}

#[test]
fn undo_and_redo_put_deep_pixels_back_exactly() {
    for depth in [Depth::U16, Depth::F32] {
        let mut canvas = white_at(depth);
        let mut b = faint_black();
        let mut history = History::new();
        let mut selection = crate::selection::SelectionManager::new();
        let mut tool = crate::app::tools::Tool::Brush;
        let mut states = vec![deep_tile(&canvas)];
        for pass in 0..3 {
            history.push_action(paint_on(&canvas, &mut b, &line(70.0, 0.5), pass));
            states.push(deep_tile(&canvas));
        }
        assert_ne!(states[0], states[3]);
        for at in (0..3).rev() {
            history.undo(&mut canvas, &mut selection, &mut tool);
            assert_eq!(deep_tile(&canvas), states[at], "{depth:?}: undo to {at}");
        }
        for state in &states[1..] {
            history.redo(&mut canvas, &mut selection, &mut tool);
            assert_eq!(&deep_tile(&canvas), state, "{depth:?}: redo");
        }
    }
}

/// Every kind of stroke paints the same at full depth as in 8 bits, give
/// or take the 8-bit rounding and dither (alpha lock aside, see below).
#[test]
fn deep_strokes_paint_what_8_bit_ones_do() {
    use crate::brush_engine::brush_options::BlendMode;
    use crate::canvas::blend_modes::LayerBlend;
    let base = || {
        let mut b = brush(BrushDynamics::default());
        b.brush_options.color = Color32::from_rgb(200, 40, 90);
        b.brush_options.opacity = 0.3;
        b
    };
    let mut eraser = base();
    eraser.brush_options.blend_mode = BlendMode::Eraser;
    let mut multiply = base();
    multiply.paint_blend = LayerBlend::Multiply;
    let mut screen = base();
    screen.paint_blend = LayerBlend::Screen;
    let mut random_hue = base();
    random_hue.dynamics = hue_random();
    let brushes = [
        ("normal", base(), false),
        ("eraser", eraser, false),
        ("multiply", multiply, false),
        ("screen", screen, false),
        ("random hue", random_hue, false),
        ("alpha locked", base(), true),
    ];
    let below = Color32::from_rgba_unmultiplied(30, 160, 220, 180);
    for (name, mut b, locked) in brushes {
        let eight = painted(below);
        let mut eight = eight;
        eight.layers[1].alpha_locked = locked;
        paint_on(&eight, &mut b, &line(70.0, 0.5), 7);
        let want = eight.get_layer_tile_data(1, 0, 1).unwrap();
        for depth in [Depth::U16, Depth::F32] {
            let mut canvas = painted(below);
            canvas.layers[1].alpha_locked = locked;
            canvas.convert_depth(depth);
            paint_on(&canvas, &mut b, &line(70.0, 0.5), 7);
            let got = canvas.get_layer_tile_data(1, 0, 1).unwrap();
            let worst = (want.iter().zip(&got))
                .flat_map(|(a, b)| (0..4).map(move |c| a[c].abs_diff(b[c])))
                .max()
                .unwrap();
            if locked {
                // The 8-bit pixels rescale their stored (sRGB-encoded)
                // values to the kept alpha, which only approximates keeping
                // the colour; at full depth it's kept exactly, in linear
                // light. Either way the alpha stays.
                assert!(
                    got.iter().all(|p| p.a() == below.a()),
                    "{name} at {depth:?}"
                );
            } else {
                assert!(worst <= 2, "{name} at {depth:?}: off by {worst}");
            }
            assert_ne!(got, vec![below; 64 * 64], "{name} at {depth:?} painted");
        }
    }
}
