//! Tests for the canvas storage: compositing, pixel writers and
//! transforms against reference results.

use super::transform::*;
use super::*;
use crate::selection::SelectionManager;
use eframe::egui::{ColorImage, Vec2};
use std::collections::HashMap;

/// The previous single-threaded implementation, kept to check the
/// parallel one maps every pixel identically.
#[allow(clippy::too_many_arguments)]
fn transform_tiles_reference(
    src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
    mut src_bounds: eframe::egui::Rect,
    params: TransformParams,
    tile_size: usize,
    canvas_width: usize,
    canvas_height: usize,
    selection: Option<&SelectionManager>,
) -> HashMap<(i32, i32), Vec<Color32>> {
    if params.scale.x.abs() < f32::EPSILON
        || params.scale.y.abs() < f32::EPSILON
        || !params.offset.is_finite()
        || !params.scale.is_finite()
        || !params.center.is_finite()
        || !params.rotation.is_finite()
    {
        return HashMap::new();
    }

    src_bounds.max.x += 1.0;
    src_bounds.max.y += 1.0;

    let corners = [
        src_bounds.min,
        eframe::egui::pos2(src_bounds.max.x, src_bounds.min.y),
        src_bounds.max,
        eframe::egui::pos2(src_bounds.min.x, src_bounds.max.y),
    ];

    let (sin_r, cos_r) = params.rotation.sin_cos();
    let transform = |p: eframe::egui::Pos2| -> eframe::egui::Pos2 {
        let dx = p.x - params.center.x;
        let dy = p.y - params.center.y;
        let sx = dx * params.scale.x;
        let sy = dy * params.scale.y;
        let rx = sx * cos_r - sy * sin_r;
        let ry = sx * sin_r + sy * cos_r;
        eframe::egui::pos2(
            rx + params.center.x + params.offset.x,
            ry + params.center.y + params.offset.y,
        )
    };

    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for corner in corners.map(transform) {
        min_x = min_x.min(corner.x);
        min_y = min_y.min(corner.y);
        max_x = max_x.max(corner.x);
        max_y = max_y.max(corner.y);
    }

    let dst_min_x = min_x.floor() as i32;
    let dst_min_y = min_y.floor() as i32;
    let dst_max_x = max_x.ceil() as i32;
    let dst_max_y = max_y.ceil() as i32;
    let tile_size_i32 = tile_size as i32;
    let center_offset_x = params.center.x + params.offset.x;
    let center_offset_y = params.center.y + params.offset.y;
    let inv_scale_x = 1.0 / params.scale.x;
    let inv_scale_y = 1.0 / params.scale.y;
    let estimated_dst_tiles =
        ((dst_max_x - dst_min_x) * (dst_max_y - dst_min_y)) / (tile_size_i32 * tile_size_i32) + 4;
    let mut dst_tiles = HashMap::with_capacity(estimated_dst_tiles.max(0) as usize);

    for y in dst_min_y..dst_max_y {
        if y < 0 || y >= canvas_height as i32 {
            continue;
        }
        for x in dst_min_x..dst_max_x {
            if x < 0 || x >= canvas_width as i32 {
                continue;
            }
            let dx = x as f32 - center_offset_x;
            let dy = y as f32 - center_offset_y;
            let rx = dx * cos_r + dy * sin_r;
            let ry = -dx * sin_r + dy * cos_r;
            let src_x = (rx * inv_scale_x + params.center.x).round() as i32;
            let src_y = (ry * inv_scale_y + params.center.y).round() as i32;

            if src_x < src_bounds.min.x.floor() as i32
                || src_x >= src_bounds.max.x.ceil() as i32
                || src_y < src_bounds.min.y.floor() as i32
                || src_y >= src_bounds.max.y.ceil() as i32
            {
                continue;
            }

            let pixel = sample_source_tile(src_tiles, src_x, src_y, tile_size, selection);
            if pixel == Color32::TRANSPARENT {
                continue;
            }

            let ntx = x.div_euclid(tile_size_i32);
            let nty = y.div_euclid(tile_size_i32);
            let npx = (x - ntx * tile_size_i32) as usize;
            let npy = (y - nty * tile_size_i32) as usize;
            let dst_data = dst_tiles
                .entry((ntx, nty))
                .or_insert_with(|| vec![Color32::TRANSPARENT; tile_size * tile_size]);
            dst_data[npy * tile_size + npx] = pixel;
        }
    }

    dst_tiles
}

#[test]
fn parallel_transform_matches_the_reference() {
    let tile_size = 16;
    let mut src: HashMap<(i32, i32), Vec<Color32>> = HashMap::new();
    for ty in 0..4 {
        for tx in 0..4 {
            let data = (0..tile_size * tile_size)
                .map(|i| {
                    let v = (i as u32).wrapping_mul(2654435761) ^ ((tx * 7 + ty * 13) as u32);
                    if v.is_multiple_of(5) {
                        Color32::TRANSPARENT
                    } else {
                        Color32::from_rgba_premultiplied(
                            v as u8,
                            (v >> 8) as u8,
                            (v >> 16) as u8,
                            255,
                        )
                    }
                })
                .collect();
            src.insert((tx, ty), data);
        }
    }
    let bounds = source_bounds_and_tiles(&src, tile_size, None).unwrap().0;
    let mut selection = SelectionManager::new();
    selection.start_selection(Vec2::new(5.0, 3.0), crate::selection::SelectionType::Circle);
    selection.update_selection(Vec2::new(50.0, 44.0));
    selection.end_selection();
    let cases = [
        TransformParams::new(
            Vec2::new(7.0, -3.0),
            0.0,
            Vec2::new(1.0, 1.0),
            Vec2::new(32.0, 32.0),
        ),
        TransformParams::new(
            Vec2::new(-4.5, 9.25),
            0.7,
            Vec2::new(1.3, 0.8),
            Vec2::new(30.0, 28.0),
        ),
        TransformParams::new(
            Vec2::new(20.0, 20.0),
            -2.1,
            Vec2::new(-0.6, 1.7),
            Vec2::new(10.0, 50.0),
        ),
    ];
    // Whole-pixel moves copy exactly, like the old nearest-neighbour code.
    // (The reference clips at the canvas edge; the real transform keeps
    // off-canvas pixels too, so compare what's on the canvas.)
    let on_canvas =
        |tiles: HashMap<(i32, i32), Vec<Color32>>| -> HashMap<(i32, i32), Vec<Color32>> {
            tiles
                .into_iter()
                .filter_map(|((tx, ty), mut data)| {
                    for (i, p) in data.iter_mut().enumerate() {
                        let x = tx * tile_size as i32 + (i % tile_size) as i32;
                        let y = ty * tile_size as i32 + (i / tile_size) as i32;
                        if !(0..70).contains(&x) || !(0..60).contains(&y) {
                            *p = Color32::TRANSPARENT;
                        }
                    }
                    data.iter().any(|p| p.a() > 0).then_some(((tx, ty), data))
                })
                .collect()
        };
    for sel in [None, Some(&selection)] {
        let fast = transform_tiles(&src, bounds, cases[0], tile_size, 70, 60, sel);
        let reference = transform_tiles_reference(&src, bounds, cases[0], tile_size, 70, 60, sel);
        assert_eq!(on_canvas(fast), on_canvas(reference));
    }
    // Moved partly off the canvas: those pixels are kept, not cut.
    let off = TransformParams::new(Vec2::new(-30.0, 0.0), 0.0, Vec2::new(1.0, 1.0), Vec2::ZERO);
    let moved = transform_tiles(&src, bounds, off, tile_size, 70, 60, None);
    assert!(moved.keys().any(|&(tx, _)| tx < 0), "off-canvas tiles kept");
    // Rotated/scaled/flipped transforms resample smoothly instead.
    for params in &cases[1..] {
        assert!(!transform_tiles(&src, bounds, *params, tile_size, 70, 60, None).is_empty());
    }
}

fn pixel_at(tiles: &HashMap<(i32, i32), Vec<Color32>>, x: i32, y: i32, ts: i32) -> Color32 {
    tiles
        .get(&(x.div_euclid(ts), y.div_euclid(ts)))
        .map_or(Color32::TRANSPARENT, |t| {
            t[(y.rem_euclid(ts) * ts + x.rem_euclid(ts)) as usize]
        })
}

fn pattern_tiles(ts: usize) -> HashMap<(i32, i32), Vec<Color32>> {
    let data = (0..ts * ts)
        .map(|i| {
            Color32::from_rgba_premultiplied((i * 3) as u8, (i * 7) as u8, (i * 11) as u8, 255)
        })
        .collect();
    HashMap::from([((0, 0), data)])
}

#[test]
fn identity_distort_copies_pixels_exactly() {
    let ts = 16;
    let src = pattern_tiles(ts);
    let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
    let area =
        eframe::egui::Rect::from_min_max(bounds.min, bounds.max + eframe::egui::vec2(1.0, 1.0));
    let params = TransformParams::distorted(Distort {
        src: area,
        dst: rect_corners(area),
    });
    let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
    assert_eq!(out.get(&(0, 0)), src.get(&(0, 0)));
}

#[test]
fn a_straight_warp_grid_changes_no_pixel() {
    use super::warp::{Warp, WarpGrid};
    let ts = 16;
    let src = pattern_tiles(ts);
    // The box as the tool has it: pixel positions, last one included.
    let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
    for n in 2..=6 {
        let params = TransformParams::warped(Warp {
            src: bounds,
            grid: WarpGrid::regular(bounds, n),
        });
        let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
        let (got, want) = (&out[&(0, 0)], &src[&(0, 0)]);
        let worst = got
            .iter()
            .zip(want)
            .map(|(a, b)| {
                a.to_array()
                    .iter()
                    .zip(b.to_array())
                    .map(|(x, y)| x.abs_diff(y))
                    .max()
                    .unwrap()
            })
            .max()
            .unwrap();
        assert!(worst <= 1, "{n} points: off by {worst}");
    }
}

#[test]
fn a_warp_bends_the_picture_through_its_points() {
    use super::warp::{Warp, WarpGrid};
    let ts = 64;
    // A white square 8..56 on a transparent tile.
    let mut tile = vec![Color32::TRANSPARENT; ts * ts];
    for y in 8..56 {
        for x in 8..56 {
            tile[y * ts + x] = Color32::WHITE;
        }
    }
    let src = HashMap::from([((0, 0), tile)]);
    let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
    let mut grid = WarpGrid::regular(bounds, 4);
    // Pull the top edge's two inner points up by 6: the top bulges.
    grid.points[1].y -= 6.0;
    grid.points[2].y -= 6.0;
    let params = TransformParams::warped(Warp { src: bounds, grid });
    let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
    let px = |x: usize, y: usize| out.get(&(0, 0)).map_or(0, |t| t[y * ts + x].a());
    assert!(px(32, 4) > 200, "bulged up in the middle");
    assert_eq!(px(9, 3), 0, "the corners stay put");
    assert!(
        px(32, 40) == 255 && px(9, 50) == 255,
        "the rest is still there"
    );
    // The preview's mesh and the render agree: every mesh vertex's source
    // point is found again at its canvas point.
    let inverse = params.inverse_map(None).unwrap();
    for (u, v) in [(0.3, 0.1), (0.5, 0.02), (0.7, 0.5)] {
        let p = Vec2::new(
            bounds.min.x + u * bounds.width(),
            bounds.min.y + v * bounds.height(),
        );
        let back = inverse.map(params.forward(p)).unwrap();
        assert!((back - p).length() < 0.2, "{p:?} -> {back:?}");
    }
}

/// Every painted output pixel lies in the corners' quad, give or take the
/// soft edge (half a source pixel, a few pixels where it's magnified); a
/// mirrored ghost would land far outside.
fn assert_inside_quad(out: &HashMap<(i32, i32), Vec<Color32>>, ts: usize, quad: [Vec2; 4]) {
    for (&(tx, ty), data) in out {
        for (i, p) in data.iter().enumerate() {
            if p.a() == 0 {
                continue;
            }
            let c = Vec2::new(
                (tx * ts as i32 + (i % ts) as i32) as f32 + 0.5,
                (ty * ts as i32 + (i / ts) as i32) as f32 + 0.5,
            );
            let inside = (0..4).all(|k| {
                let (a, b) = (quad[k], quad[(k + 1) % 4]);
                let edge = b - a;
                let len = edge.length();
                // Signed distance, positive inside for a clockwise quad.
                (edge.x * (c - a).y - edge.y * (c - a).x) / len >= -4.0
            });
            assert!(inside, "pixel at {c:?} outside {quad:?}");
        }
    }
}

#[test]
fn a_strong_perspective_stays_inside_its_corners() {
    let ts = 16;
    let src = pattern_tiles(ts);
    let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
    let area =
        eframe::egui::Rect::from_min_max(bounds.min, bounds.max + eframe::egui::vec2(1.0, 1.0));
    // Near-vanishing edges, magnified in places: nothing lands outside.
    let quads = [
        [
            Vec2::new(20.0, 20.0),
            Vec2::new(22.0, 20.5),
            Vec2::new(60.0, 60.0),
            Vec2::new(2.0, 58.0),
        ],
        [
            Vec2::new(10.0, 10.0),
            Vec2::new(60.0, 12.0),
            Vec2::new(56.0, 20.0),
            Vec2::new(12.0, 60.0),
        ],
    ];
    for dst in quads {
        assert!(is_convex_quad(&dst));
        let params = TransformParams::distorted(Distort { src: area, dst });
        let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
        assert!(!out.is_empty());
        assert_inside_quad(&out, ts, dst);
    }
}

#[test]
fn perspective_goes_back_to_where_it_came_from() {
    let area = eframe::egui::Rect::from_min_max(
        eframe::egui::pos2(0.0, 0.0),
        eframe::egui::pos2(40.0, 20.0),
    );
    let dst = [
        Vec2::new(5.0, 3.0),
        Vec2::new(70.0, -4.0),
        Vec2::new(60.0, 50.0),
        Vec2::new(-2.0, 30.0),
    ];
    let params = TransformParams::distorted(Distort { src: area, dst });
    // Corners land on the corners.
    for (c, want) in rect_corners(area).into_iter().zip(dst) {
        assert!((params.forward(c) - want).length() < 1e-3);
    }
    let inverse = params.inverse_map(None).unwrap();
    for i in 0..=8 {
        for j in 0..=8 {
            let p = Vec2::new(i as f32 * 5.0, j as f32 * 2.5);
            let back = inverse.map(params.forward(p)).unwrap();
            assert!((back - p).length() < 1e-2, "{p:?} -> {back:?}");
        }
    }
    // Perspective foreshortens: the middle of the top edge isn't halfway
    // between its corners.
    let even = (dst[0] + dst[1]) / 2.0;
    assert!((params.forward(Vec2::new(20.0, 0.0)) - even).length() > 0.5);
}

#[test]
fn only_convex_quads_are_accepted() {
    let square = [
        Vec2::new(0.0, 0.0),
        Vec2::new(10.0, 0.0),
        Vec2::new(10.0, 10.0),
        Vec2::new(0.0, 10.0),
    ];
    assert!(is_convex_quad(&square));
    let mut folded = square;
    folded[2] = Vec2::new(-5.0, -5.0);
    assert!(!is_convex_quad(&folded), "corner dragged across the box");
    let mut dented = square;
    dented[2] = Vec2::new(4.0, 4.0);
    assert!(!is_convex_quad(&dented), "concave");
    let mut flat = square;
    flat[1] = Vec2::new(0.0, 0.0);
    assert!(!is_convex_quad(&flat));
}

#[test]
fn quarter_turn_maps_pixels_exactly() {
    let ts = 16;
    let src = pattern_tiles(ts);
    let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
    // About the tile centre (8, 8): pixel (x, y) goes to (15 - y, x).
    let params = TransformParams::new(
        Vec2::ZERO,
        std::f32::consts::FRAC_PI_2,
        Vec2::new(1.0, 1.0),
        Vec2::new(8.0, 8.0),
    );
    let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
    for (x, y) in [(0, 0), (3, 1), (15, 15), (7, 12)] {
        assert_eq!(
            pixel_at(&out, 15 - y, x, ts as i32),
            pixel_at(&src, x, y, ts as i32),
            "({x},{y})"
        );
    }
}

#[test]
fn upscaling_blends_between_pixels() {
    let ts = 16;
    let mut data = vec![Color32::BLACK; ts * ts];
    data[0] = Color32::WHITE;
    let src = HashMap::from([((0, 0), data)]);
    let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
    let params = TransformParams::new(Vec2::ZERO, 0.0, Vec2::new(4.0, 4.0), Vec2::ZERO);
    let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
    let mid = pixel_at(&out, 4, 1, ts as i32);
    assert!(mid.r() > 0 && mid.r() < 255, "smooth edge, got {mid:?}");
}

const T: usize = 8;

/// An 8x8 canvas (one tile): white background, layer 1 above it.
fn one_tile_canvas() -> Canvas {
    Canvas::new(T, T, Color32::WHITE, T)
}

fn fill(canvas: &Canvas, idx: usize, color: Color32) {
    canvas.set_layer_tile_data(idx, 0, 0, vec![color; T * T]);
}

fn pixel(canvas: &Canvas) -> Color32 {
    let mut img = ColorImage::new([1, 1], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, 1, 1, &mut img, 1);
    img.pixels[0]
}

#[test]
fn downsampled_composite_matches_composite_then_downsample() {
    use crate::canvas::blend::downsample;
    let mut seed = 0x9e37_79b9_u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut random_tile = |n: usize| -> Vec<Color32> {
        (0..n)
            .map(|_| {
                let v = next();
                let a = match v % 4 {
                    0 => 0,
                    1 => 255,
                    _ => (v >> 24) as u8,
                };
                let pm = |c: u32| ((c & 0xff) * a as u32 / 255) as u8;
                Color32::from_rgba_premultiplied(pm(v >> 1), pm(v >> 9), pm(v >> 17), a)
            })
            .collect()
    };
    // 100 px canvas, 64 px tiles: tile (1, 1) is a 36 px edge tile, so
    // blocks at its far edges are partial.
    for case in 0..3 {
        let mut canvas = Canvas::new(100, 100, Color32::WHITE, 64);
        for (tx, ty) in [(0, 0), (1, 1)] {
            canvas.set_layer_tile_data(1, tx, ty, random_tile(64 * 64));
        }
        match case {
            0 => {} // background + one opaque layer: the fast path
            1 => {
                canvas.set_layer_tile_data(0, 1, 1, random_tile(64 * 64));
                canvas.layers[1].opacity = 0.5; // general path
            }
            _ => {
                let owner = canvas.layers[1].id;
                let m = canvas.insert_new_layer(2, "m".into(), LayerKind::Mask { owner }, None);
                let mi = canvas.layer_index_of(m).unwrap();
                canvas.set_layer_tile_data(mi, 1, 1, random_tile(64 * 64));
            }
        }
        for (tx, ty, rect) in [
            (0, 0, [8, 16, 40, 48]),
            (1, 1, [0, 0, 36, 36]),
            (1, 1, [4, 8, 36, 36]),
        ] {
            for level in 1..=3u32 {
                let block = 1usize << level;
                let mut full = ColorImage::new([0, 0], Color32::TRANSPARENT);
                canvas.write_tile_rect_to_color_image(tx, ty, rect, &mut full, None);
                let expected = downsample(&full, level);
                let mut fused = ColorImage::new([0, 0], Color32::TRANSPARENT);
                canvas.write_tile_rect_downsampled(tx, ty, rect, block, &mut fused, None);
                assert_eq!(
                    fused.size, expected.size,
                    "case {case} rect {rect:?} level {level}"
                );
                for (i, (a, b)) in fused.pixels.iter().zip(&expected.pixels).enumerate() {
                    let close = a
                        .to_array()
                        .iter()
                        .zip(b.to_array())
                        .all(|(x, y)| x.abs_diff(y) <= 1);
                    assert!(
                        close,
                        "case {case} rect {rect:?} level {level} px {i}: {a:?} vs {b:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn half_black_over_white_depends_on_the_blend_space() {
    let mut canvas = one_tile_canvas();
    fill(&canvas, 1, Color32::BLACK);
    canvas.layers[1].opacity = 0.5;
    // Linear light: half the light of white, re-encoded to sRGB.
    assert_eq!(pixel(&canvas).r(), 188);
    // Gamma: half of the stored value.
    canvas.blend_space = BlendSpace::Gamma;
    assert_eq!(pixel(&canvas).r(), 128);
}

#[test]
fn gamma_fast_path_matches_the_tree_compositor() {
    let mut seed = 0x1234_5678_u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let data: Vec<Color32> = (0..T * T)
        .map(|_| {
            let v = next();
            let a = (v >> 24) as u8;
            let pm = |c: u32| ((c & 0xff) * a as u32 / 255) as u8;
            Color32::from_rgba_premultiplied(pm(v), pm(v >> 8), pm(v >> 16), a)
        })
        .collect();
    let mut canvas = one_tile_canvas();
    canvas.blend_space = BlendSpace::Gamma;
    canvas.set_layer_tile_data(1, 0, 0, data);
    let mut fast = ColorImage::new([0, 0], Color32::TRANSPARENT);
    canvas.write_tile_rect_to_color_image(0, 0, [0, 0, T, T], &mut fast, None);
    // Force the tree compositor with an empty folder.
    canvas.insert_new_layer(2, "empty".into(), LayerKind::Group, None);
    let mut tree = ColorImage::new([0, 0], Color32::TRANSPARENT);
    canvas.write_tile_rect_to_color_image(0, 0, [0, 0, T, T], &mut tree, None);
    assert_eq!(fast.pixels, tree.pixels);
}

#[test]
fn multiply_layer_multiplies_stored_values_in_gamma_space() {
    let mut canvas = one_tile_canvas();
    canvas.blend_space = BlendSpace::Gamma;
    fill(&canvas, 1, Color32::from_rgb(200, 100, 50));
    canvas.insert_new_layer(2, "shade".into(), LayerKind::Paint, None);
    fill(&canvas, 2, Color32::from_rgb(128, 128, 128));
    canvas.layers[2].blend = LayerBlend::Multiply;
    let px = pixel(&canvas);
    // 200 * 128 / 255 = 100.4, 50.2, 25.1
    assert_eq!([px.r(), px.g(), px.b()], [100, 50, 25]);
}

#[test]
fn multiply_folder_blends_its_whole_contents() {
    let mut canvas = one_tile_canvas();
    canvas.blend_space = BlendSpace::Gamma;
    fill(&canvas, 1, Color32::from_rgb(200, 100, 50));
    let folder = canvas.insert_new_layer(2, "folder".into(), LayerKind::Group, None);
    canvas.layers[2].blend = LayerBlend::Multiply;
    let inner = canvas.insert_new_layer(3, "inner".into(), LayerKind::Paint, Some(folder));
    let inner_idx = canvas.layer_index_of(inner).unwrap();
    fill(&canvas, inner_idx, Color32::from_rgb(128, 128, 128));
    let px = pixel(&canvas);
    assert_eq!([px.r(), px.g(), px.b()], [100, 50, 25]);
}

#[test]
fn new_mask_shows_everything_and_black_hides() {
    let mut canvas = one_tile_canvas();
    fill(&canvas, 1, Color32::RED);
    let owner = canvas.layers[1].id;
    let mask = canvas.insert_new_layer(2, "mask".into(), LayerKind::Mask { owner }, None);
    assert_eq!(
        pixel(&canvas),
        Color32::RED,
        "a mask without tiles shows everything"
    );

    // A freshly created mask tile starts white (shows everything).
    let mask_idx = canvas.layer_index_of(mask).unwrap();
    canvas.ensure_layer_tile_exists(mask_idx, 0, 0);
    assert_eq!(pixel(&canvas), Color32::RED);

    fill(&canvas, mask_idx, Color32::BLACK);
    assert_eq!(pixel(&canvas), Color32::WHITE, "black mask hides the layer");
    fill(&canvas, mask_idx, Color32::TRANSPARENT);
    assert_eq!(
        pixel(&canvas),
        Color32::WHITE,
        "erased mask hides the layer"
    );

    canvas.layers[mask_idx].visible = false;
    assert_eq!(
        pixel(&canvas),
        Color32::RED,
        "a disabled mask masks nothing"
    );
}

#[test]
fn folder_opacity_applies_to_its_contents_as_one() {
    // Reference: a single red layer at 50% over white.
    let mut flat = one_tile_canvas();
    fill(&flat, 1, Color32::RED);
    flat.layers[1].opacity = 0.5;
    let expected = pixel(&flat);

    // Blue under red, both opaque, in a 50% folder: the folder's composite
    // (all red) is what gets faded, so blue must not show through.
    let mut canvas = one_tile_canvas();
    let folder = canvas.insert_new_layer(2, "folder".into(), LayerKind::Group, None);
    canvas.layers[2].opacity = 0.5;
    fill(&canvas, 1, Color32::BLUE);
    canvas.layers[1].parent = Some(folder);
    let top = canvas.insert_new_layer(2, "top".into(), LayerKind::Paint, Some(folder));
    let top_idx = canvas.layer_index_of(top).unwrap();
    fill(&canvas, top_idx, Color32::RED);
    assert_eq!(pixel(&canvas), expected);
}

#[test]
fn hidden_folder_hides_its_contents() {
    let mut canvas = one_tile_canvas();
    let folder = canvas.insert_new_layer(2, "folder".into(), LayerKind::Group, None);
    fill(&canvas, 1, Color32::RED);
    canvas.layers[1].parent = Some(folder);
    assert_eq!(pixel(&canvas), Color32::RED);
    canvas.layers[2].visible = false;
    assert_eq!(pixel(&canvas), Color32::WHITE);
}

#[test]
fn floating_selection_sits_right_above_its_source() {
    let mut canvas = one_tile_canvas();
    canvas.insert_new_layer(2, "top".into(), LayerKind::Paint, None);
    fill(&canvas, 1, Color32::RED);
    canvas.active_layer_idx = 1;
    let mut selection = SelectionManager::new();
    selection.start_selection(
        Vec2::new(0.0, 0.0),
        crate::selection::SelectionType::Rectangle,
    );
    selection.update_selection(Vec2::new(4.0, 4.0));
    selection.end_selection();
    let idx = canvas.float_selection(&selection).unwrap();
    assert_eq!(
        idx, 2,
        "floated layer goes directly above layer 1, below 'top'"
    );
    assert_eq!(canvas.layers[3].name, "top");
}

#[test]
fn zero_sized_region_clears_output() {
    let canvas = Canvas::new(8, 8, Color32::WHITE, 4);
    let mut image = ColorImage::new([2, 2], Color32::BLACK);

    canvas.write_region_to_color_image(0, 0, 0, 8, &mut image, 1);

    assert_eq!(image.size, [0, 0]);
    assert!(image.pixels.is_empty());
}

/// Deterministic, dependency-free hash (FNV-1a) over raw RGBA bytes. See
/// the equivalent helper in brush_engine::brush::tests for why: a
/// golden-master checksum lets a refactor of write_region_to_color_image
/// be proven pixel-identical without a display to compare renders on.
fn checksum_pixels(pixels: &[Color32]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for p in pixels {
        for b in p.to_array() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// 8x8 canvas (2x2 grid of 4x4 tiles), two layers, each tile given a
/// distinct semi-transparent pattern so a tile-indexing bug in either
/// layer would change the checksum.
fn build_region_test_canvas() -> Canvas {
    let canvas = Canvas::new(8, 8, Color32::from_rgba_unmultiplied(230, 230, 230, 255), 4);
    for (li, base) in [(0usize, 10u8), (1usize, 60u8)] {
        for (tx, ty) in [(0i32, 0i32), (1, 0), (0, 1), (1, 1)] {
            let mut data = vec![Color32::TRANSPARENT; 16];
            for (i, px) in data.iter_mut().enumerate() {
                let v = base
                    .wrapping_add((tx as u8) * 40)
                    .wrapping_add((ty as u8) * 20)
                    .wrapping_add(i as u8 * 3);
                *px =
                    Color32::from_rgba_unmultiplied(v, v.wrapping_add(50), v.wrapping_add(90), 180);
            }
            canvas.set_layer_tile_data(li, tx, ty, data);
        }
    }
    canvas
}

/// Golden-master check for the single-tile fast path
/// (`try_write_single_tile_fast`, step == 1, region within one tile).
#[test]
fn write_region_single_tile_step1_is_stable() {
    let canvas = build_region_test_canvas();
    let mut image = ColorImage::new([4, 4], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, 4, 4, &mut image, 1);
    assert_eq!(
        checksum_pixels(&image.pixels),
        0xf9e95eb826f8c02f,
        "GOLDEN_PLACEHOLDER:write_region_single_tile_step1_is_stable"
    );
}

/// Golden-master check for the single-tile downsampling path (step > 1,
/// still within one tile: falls through try_write_single_tile_fast into
/// the "Fast path: Single tile access" block's step != 1 branch).
#[test]
fn write_region_single_tile_step2_is_stable() {
    let canvas = build_region_test_canvas();
    let mut image = ColorImage::new([2, 2], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, 4, 4, &mut image, 2);
    assert_eq!(
        checksum_pixels(&image.pixels),
        0x14933deaac974042,
        "GOLDEN_PLACEHOLDER:write_region_single_tile_step2_is_stable"
    );
}

/// Golden-master check for the multi-tile fallback path, step == 1,
/// covering the full 2x2 tile grid.
#[test]
fn write_region_multi_tile_step1_is_stable() {
    let canvas = build_region_test_canvas();
    let mut image = ColorImage::new([8, 8], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, 8, 8, &mut image, 1);
    assert_eq!(
        checksum_pixels(&image.pixels),
        0xa3d9415bbd039077,
        "GOLDEN_PLACEHOLDER:write_region_multi_tile_step1_is_stable"
    );
}

/// Golden-master check for the multi-tile fallback path with step > 1.
#[test]
fn write_region_multi_tile_step2_is_stable() {
    let canvas = build_region_test_canvas();
    let mut image = ColorImage::new([4, 4], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, 8, 8, &mut image, 2);
    assert_eq!(
        checksum_pixels(&image.pixels),
        0xd25bf6d6129a85bc,
        "GOLDEN_PLACEHOLDER:write_region_multi_tile_step2_is_stable"
    );
}

/// Golden-master check for a region that straddles tile boundaries
/// without being canvas-aligned (offset start, spans 3 of the 4 tiles).
#[test]
fn write_region_offset_multi_tile_is_stable() {
    let canvas = build_region_test_canvas();
    let mut image = ColorImage::new([5, 5], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(2, 2, 5, 5, &mut image, 1);
    assert_eq!(
        checksum_pixels(&image.pixels),
        0xff2bb4312a21f9bd,
        "GOLDEN_PLACEHOLDER:write_region_offset_multi_tile_is_stable"
    );
}

#[test]
fn transform_output_is_clipped_to_canvas() {
    let mut canvas = Canvas::new(8, 8, Color32::WHITE, 4);
    let mut data = vec![Color32::TRANSPARENT; 16];
    data[0] = Color32::BLACK;
    canvas.set_layer_tile_data(1, 0, 0, data);

    canvas.apply_transform(
        TransformParams::new(Vec2::new(-20.0, -20.0), 0.0, Vec2::splat(1.0), Vec2::ZERO),
        None,
        None,
    );

    let tiles = canvas.layers[1]
        .tiles
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(tiles.keys().all(|(tx, ty)| *tx >= 0 && *ty >= 0));
}

#[test]
fn tile_composite_from_cached_below_matches_full_composite() {
    let mut canvas = build_region_test_canvas();
    canvas.add_layer();
    canvas.add_layer();
    for (li, base) in [(2usize, 120u8), (3usize, 200u8)] {
        for (tx, ty) in [(0i32, 0i32), (1, 1)] {
            let data = (0..16)
                .map(|i| {
                    let v = base.wrapping_add(i as u8 * 7);
                    Color32::from_rgba_unmultiplied(v, 255 - v, v / 2, 40 + i as u8 * 12)
                })
                .collect();
            canvas.set_layer_tile_data(li, tx, ty, data);
        }
    }
    canvas.layers[1].opacity = 0.6;
    canvas.layers[3].opacity = 0.8;

    for active in 1..=3 {
        for step in [1, 2] {
            for (tx, ty) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
                let mut full = ColorImage::new([1, 1], Color32::TRANSPARENT);
                canvas.write_tile_to_color_image(tx, ty, &mut full, step, None);

                let below = canvas.composite_below(active, tx as i32, ty as i32);
                let mut cached = ColorImage::new([1, 1], Color32::TRANSPARENT);
                let prefix = BelowComposite {
                    first_layer: active,
                    pixels: &below,
                };
                canvas.write_tile_to_color_image(tx, ty, &mut cached, step, Some(prefix));

                assert_eq!(
                    full.pixels, cached.pixels,
                    "active={active} step={step} tile=({tx},{ty})"
                );
            }
        }
    }
}

fn empty_action() -> UndoAction {
    UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    }
}

#[test]
fn paint_region_paints_inside_its_bounds_across_tiles() {
    let canvas = Canvas::new(128, 128, Color32::WHITE, 64);
    let mut undo = empty_action();
    let red = Color32::from_rgb(255, 0, 0);
    let rect = canvas.paint_region(1, [60, 10, 70, 20], |_, _| red, &mut undo);
    assert!(rect.is_some());
    assert_eq!(undo.tiles.len(), 2, "one snapshot per touched tile");
    let left = canvas.get_layer_tile_data(1, 0, 0).unwrap();
    let right = canvas.get_layer_tile_data(1, 1, 0).unwrap();
    assert_eq!(left[15 * 64 + 63], red);
    assert_eq!(right[15 * 64 + 5], red);
    assert_eq!(
        right[15 * 64 + 6],
        Color32::TRANSPARENT,
        "outside the bounds"
    );
    assert_eq!(
        left[25 * 64 + 63],
        Color32::TRANSPARENT,
        "outside the bounds"
    );
}

#[test]
fn paint_region_respects_alpha_lock_and_skips_unchanged_tiles() {
    let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
    let mut data = vec![Color32::TRANSPARENT; 64 * 64];
    data[0] = Color32::from_rgb(0, 0, 255);
    canvas.set_layer_tile_data(1, 0, 0, data);
    canvas.layers[1].alpha_locked = true;
    let mut undo = empty_action();
    let red = Color32::from_rgb(255, 0, 0);
    canvas.paint_region(1, [0, 0, 64, 64], |_, _| red, &mut undo);
    let tile = canvas.get_layer_tile_data(1, 0, 0).unwrap();
    assert_eq!(tile[0], red, "painted pixel recoloured");
    assert_eq!(tile[1], Color32::TRANSPARENT, "empty pixel stays empty");

    let mut undo = empty_action();
    let none = canvas.paint_region(1, [0, 0, 64, 64], |_, _| Color32::TRANSPARENT, &mut undo);
    assert!(none.is_none());
    assert!(undo.tiles.is_empty());
}

/// Pixel `(x, 0)` of the composite.
fn pixel_x(canvas: &Canvas, x: usize) -> Color32 {
    let mut img = ColorImage::new([1, 1], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(x, 0, 1, 1, &mut img, 1);
    img.pixels[0]
}

#[test]
fn a_clipped_layer_shows_only_over_its_base() {
    let mut canvas = one_tile_canvas();
    // The base covers the left half; the clipped layer covers everything.
    let mut base = vec![Color32::TRANSPARENT; T * T];
    for y in 0..T {
        for x in 0..T / 2 {
            base[y * T + x] = Color32::RED;
        }
    }
    canvas.set_layer_tile_data(1, 0, 0, base);
    canvas.insert_new_layer(2, "shade".into(), LayerKind::Paint, None);
    fill(&canvas, 2, Color32::BLUE);
    canvas.layers[2].clipped = true;
    assert_eq!(pixel_x(&canvas, 1), Color32::BLUE, "over the base");
    assert_eq!(pixel_x(&canvas, 6), Color32::WHITE, "outside it: nothing");
    // Unclipped, it covers everything again.
    canvas.layers[2].clipped = false;
    assert_eq!(pixel_x(&canvas, 6), Color32::BLUE);
}

#[test]
fn clipping_takes_the_base_alpha_and_hides_with_it() {
    let mut canvas = one_tile_canvas();
    canvas.blend_space = BlendSpace::Gamma;
    fill(&canvas, 1, Color32::from_rgba_unmultiplied(255, 0, 0, 128));
    canvas.insert_new_layer(2, "a".into(), LayerKind::Paint, None);
    fill(&canvas, 2, Color32::BLUE);
    canvas.layers[2].clipped = true;
    // Blue at the base's half alpha over white.
    let px = pixel(&canvas);
    assert!(px.b() > 250 && (125..=130).contains(&px.r()), "{px:?}");
    // Several clipped layers stack on the same base.
    canvas.insert_new_layer(3, "b".into(), LayerKind::Paint, None);
    fill(&canvas, 3, Color32::GREEN);
    canvas.layers[3].clipped = true;
    let px = pixel(&canvas);
    assert!(px.g() > 250 && px.b() < 130, "{px:?}");
    // A hidden base hides what's clipped to it.
    canvas.layers[1].visible = false;
    assert_eq!(pixel(&canvas), Color32::WHITE);
}

#[test]
fn a_clipped_layer_first_in_its_folder_shows_as_usual() {
    let mut canvas = one_tile_canvas();
    let folder = canvas.insert_new_layer(2, "folder".into(), LayerKind::Group, None);
    fill(&canvas, 1, Color32::BLUE);
    canvas.layers[1].parent = Some(folder);
    canvas.layers[1].clipped = true;
    assert_eq!(pixel(&canvas), Color32::BLUE);
}

#[test]
fn an_adjustment_layer_changes_what_is_below_it() {
    use crate::canvas::filters::Filter;
    let mut canvas = one_tile_canvas();
    canvas.blend_space = BlendSpace::Gamma;
    fill(&canvas, 1, Color32::RED);
    let adj = canvas.insert_new_layer(2, "invert".into(), LayerKind::Paint, None);
    let ai = canvas.layer_index_of(adj).unwrap();
    canvas.layers[ai].adjustment = Some(Filter::Invert);
    assert_eq!(pixel(&canvas), Color32::from_rgb(0, 255, 255));
    // Half opacity: half way.
    canvas.layers[ai].opacity = 0.5;
    let px = pixel(&canvas);
    assert!(
        (126..=129).contains(&px.r()) && (126..=129).contains(&px.g()),
        "{px:?}"
    );
    canvas.layers[ai].opacity = 1.0;
    // A black mask keeps it off; hidden, it does nothing.
    let m = canvas.insert_new_layer(3, "m".into(), LayerKind::Mask { owner: adj }, None);
    let mi = canvas.layer_index_of(m).unwrap();
    fill(&canvas, mi, Color32::BLACK);
    assert_eq!(pixel(&canvas), Color32::RED);
    canvas.layers[mi].visible = false;
    canvas.layers[ai].visible = false;
    assert_eq!(pixel(&canvas), Color32::RED);
    // Layers above it aren't touched.
    canvas.layers[ai].visible = true;
    let top = canvas.insert_new_layer(4, "top".into(), LayerKind::Paint, None);
    let ti = canvas.layer_index_of(top).unwrap();
    fill(&canvas, ti, Color32::BLUE);
    assert_eq!(pixel(&canvas), Color32::BLUE);
}

#[test]
fn curves_colour_balance_and_gradient_map_work_as_adjustment_layers() {
    use crate::canvas::filters::{Filter, GradientMap, ToneCurve};
    for space in [BlendSpace::Gamma, BlendSpace::Linear] {
        let mut canvas = one_tile_canvas();
        canvas.blend_space = space;
        fill(&canvas, 1, Color32::from_rgb(128, 128, 128));
        let adj = canvas.insert_new_layer(2, "adj".into(), LayerKind::Paint, None);
        let ai = canvas.layer_index_of(adj).unwrap();
        // A red curve that drops red to nothing.
        canvas.layers[ai].adjustment = Some(Filter::Curves {
            rgb: ToneCurve::default(),
            red: ToneCurve::from_points(&[[0.0, 0.0], [1.0, 0.0]]),
            green: ToneCurve::default(),
            blue: ToneCurve::default(),
        });
        let px = pixel(&canvas);
        assert!(px.r() <= 1 && px.g().abs_diff(128) <= 1, "{space:?} {px:?}");
        canvas.layers[ai].adjustment = Some(Filter::GradientMap(GradientMap::from_stops(&[
            (0.0, [0, 0, 0]),
            (0.5, [0, 200, 0]),
            (1.0, [255, 255, 255]),
        ])));
        let px = pixel(&canvas);
        assert!(
            px.r() <= 2 && px.g() >= 195 && px.b() <= 2,
            "{space:?} {px:?}"
        );
        canvas.layers[ai].adjustment = Some(Filter::ColourBalance {
            shadows: [0.0; 3],
            midtones: [0.6, 0.0, 0.0],
            highlights: [0.0; 3],
            preserve_luminosity: false,
        });
        let px = pixel(&canvas);
        assert!(
            px.r() > 180 && px.g().abs_diff(128) <= 1,
            "{space:?} {px:?}"
        );
    }
}

#[test]
fn fills_and_backgrounds_keep_their_exact_colour() {
    // A mid grey used to come out much lighter (converted to sRGB twice).
    let grey = Color32::from_rgb(100, 100, 100);
    let canvas = Canvas::new(8, 8, grey, 8);
    // A background tile starts as the background colour.
    canvas.ensure_layer_tile(0, 0, 0);
    assert_eq!(canvas.get_layer_tile_data(0, 0, 0).unwrap()[0], grey);
    let mut undo = empty_action();
    let mut data = vec![0u8; 64];
    data[0] = 255;
    data[1] = 128;
    let mask = crate::selection::SelectionMask::new(0, 0, 8, 8, data);
    canvas.paint_mask(1, &mask, grey, &mut undo);
    let tile = canvas.get_layer_tile_data(1, 0, 0).unwrap();
    assert_eq!(tile[0], grey);
    assert_eq!(tile[1], Color32::from_rgba_unmultiplied(100, 100, 100, 128));
}

/// Zoomed out, a stroke shows as tiles averaged on the CPU; once it ends,
/// as full tiles the GPU averages (in linear light). Both must look the
/// same, or the grain and soft edges shift when the pen lifts.
#[test]
fn a_gamma_documents_zoomed_out_preview_matches_the_final_picture() {
    use crate::canvas::blend::downsample;
    let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
    canvas.blend_space = BlendSpace::Gamma;
    // Grainy half-transparent paint, like a textured brush.
    let tile: Vec<Color32> = (0..64 * 64)
        .map(|i| {
            let a = if (i * 7919) % 3 == 0 { 40 } else { 200 };
            Color32::from_rgba_unmultiplied(30, 60, 160, a)
        })
        .collect();
    canvas.set_layer_tile_data(1, 0, 0, tile);
    for level in 1..=3u32 {
        let mut full = ColorImage::new([0, 0], Color32::TRANSPARENT);
        canvas.write_tile_rect_to_color_image(0, 0, [0, 0, 64, 64], &mut full, None);
        let expected = downsample(&full, level);
        let mut preview = ColorImage::new([0, 0], Color32::TRANSPARENT);
        canvas.write_tile_rect_downsampled(0, 0, [0, 0, 64, 64], 1 << level, &mut preview, None);
        let worst = preview
            .pixels
            .iter()
            .zip(&expected.pixels)
            .map(|(a, b)| {
                a.to_array()
                    .iter()
                    .zip(b.to_array())
                    .map(|(x, y)| x.abs_diff(y))
                    .max()
                    .unwrap()
            })
            .max()
            .unwrap();
        assert!(worst <= 1, "level {level}: preview off by {worst}");
    }
}
