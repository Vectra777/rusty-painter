use criterion::{Criterion, criterion_group, criterion_main};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;
use rusty_painter::{
    brush_engine::{
        brush::Brush,
        stroke::{StrokeContext, StrokeState, StrokeTiles},
    },
    canvas::{Canvas, history::UndoAction},
};

fn bench_soft_dab(c: &mut Criterion) {
    let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let canvas = Canvas::new(512, 512, Color32::WHITE, 64);
    let mut brush = Brush::new(
        48.0,
        50.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, 255),
        20.0,
    );
    let mut undo_action = UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    };
    let mut stroke_tiles = StrokeTiles::default();

    // Warm up the mask cache and tile allocation so the measurement focuses on per-dab work.
    let mut stroke = StrokeState::new();
    let mut context = StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
    stroke.add_point(&mut brush, Vec2 { x: 256.0, y: 256.0 }, 1.0, &mut context);
    undo_action.tiles.clear();
    stroke_tiles = StrokeTiles::default();

    c.bench_function("soft_dab_512px", |b| {
        b.iter(|| {
            let mut stroke = StrokeState::new();
            undo_action.tiles.clear();
            stroke_tiles = StrokeTiles::default();

            let mut context =
                StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
            stroke.add_point(&mut brush, Vec2 { x: 256.0, y: 256.0 }, 1.0, &mut context);
            stroke.add_point(&mut brush, Vec2 { x: 280.0, y: 256.0 }, 1.0, &mut context);
        });
    });
}

/// A curved 60-sample stroke with varying pressure, so dab diameters and
/// sub-pixel positions change every sample like real tablet input.
fn bench_pressure_stroke(c: &mut Criterion) {
    let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let canvas = Canvas::new(1024, 1024, Color32::WHITE, 64);
    let mut brush = Brush::new(
        60.0,
        40.0,
        Color32::from_rgba_unmultiplied(30, 60, 200, 255),
        10.0,
    );
    let points: Vec<(Vec2, f32)> = (0..60)
        .map(|i| {
            let t = i as f32 / 59.0;
            let pos = Vec2::new(150.0 + t * 700.0, 500.0 + (t * 9.0).sin() * 200.3);
            (pos, 0.3 + 0.7 * (t * std::f32::consts::PI).sin())
        })
        .collect();

    c.bench_function("pressure_stroke_60_samples", |b| {
        b.iter(|| {
            let mut undo_action = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut stroke_tiles = StrokeTiles::default();
            let mut stroke = StrokeState::new();
            let mut context =
                StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
            for &(pos, pressure) in &points {
                stroke.add_point(&mut brush, pos, pressure, &mut context);
            }
        });
    });
}

/// The pressure stroke on a pool the size the app uses (every logical
/// core): small per-sample batches spread over many threads, where rayon's
/// scheduling overhead shows up.
fn bench_pressure_stroke_app_pool(c: &mut Criterion) {
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let canvas = Canvas::new(1024, 1024, Color32::WHITE, 64);
    let mut brush = Brush::new(
        60.0,
        40.0,
        Color32::from_rgba_unmultiplied(30, 60, 200, 255),
        10.0,
    );
    let points: Vec<(Vec2, f32)> = (0..60)
        .map(|i| {
            let t = i as f32 / 59.0;
            let pos = Vec2::new(150.0 + t * 700.0, 500.0 + (t * 9.0).sin() * 200.3);
            (pos, 0.3 + 0.7 * (t * std::f32::consts::PI).sin())
        })
        .collect();

    c.bench_function("pressure_stroke_app_pool", |b| {
        b.iter(|| {
            let mut undo_action = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut stroke_tiles = StrokeTiles::default();
            let mut stroke = StrokeState::new();
            let mut context =
                StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
            for &(pos, pressure) in &points {
                stroke.add_point(&mut brush, pos, pressure, &mut context);
            }
        });
    });
}

/// A 1500 px soft brush dragged across a 4000 px canvas: the case a
/// preview at a coarser level exists for.
fn bench_huge_brush_stroke(c: &mut Criterion) {
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let canvas = Canvas::new(4000, 4000, Color32::WHITE, 64);
    let mut brush = Brush::new(
        1500.0,
        40.0,
        Color32::from_rgba_unmultiplied(200, 60, 30, 255),
        10.0,
    );
    let points: Vec<Vec2> = (0..40)
        .map(|i| {
            Vec2::new(
                250.0 + i as f32 * 90.0,
                2000.0 + (i as f32 * 0.2).sin() * 600.0,
            )
        })
        .collect();

    let mut group = c.benchmark_group("huge_brush");
    group.sample_size(10);
    group.bench_function("stroke_1500px_40_samples", |b| {
        b.iter(|| {
            let mut undo_action = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut stroke_tiles = StrokeTiles::default();
            let mut stroke = StrokeState::new();
            let mut context =
                StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
            for &pos in &points {
                stroke.add_point(&mut brush, pos, 1.0, &mut context);
            }
        });
    });
    group.finish();
}

/// Recompositing the ~600 display tiles a 1500 px dab dirties (what one frame
/// of a huge-brush stroke costs on the display side), across the pool.
fn bench_composite_dirty_tiles(c: &mut Criterion) {
    use eframe::egui::ColorImage;
    use rayon::prelude::*;
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let canvas = Canvas::new(4000, 4000, Color32::WHITE, 64);
    let tiles: Vec<(usize, usize)> = (20..44)
        .flat_map(|ty| (20..44).map(move |tx| (tx, ty)))
        .collect();
    c.bench_function("composite_576_tiles", |b| {
        b.iter(|| {
            pool.install(|| {
                tiles.par_iter().for_each(|&(tx, ty)| {
                    let mut img = ColorImage::new([0, 0], Color32::TRANSPARENT);
                    canvas.write_tile_to_color_image(tx, ty, &mut img, 1, None);
                })
            })
        });
    });
}

/// The same tiles when a stroke only changed a 20 px square of each (what a
/// medium brush damages per tile per frame): only that part is recomposited.
fn bench_composite_damaged_rects(c: &mut Criterion) {
    use eframe::egui::ColorImage;
    use rayon::prelude::*;
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let canvas = Canvas::new(4000, 4000, Color32::WHITE, 64);
    let tiles: Vec<(usize, usize)> = (20..44)
        .flat_map(|ty| (20..44).map(move |tx| (tx, ty)))
        .collect();
    c.bench_function("composite_576_tile_rects_20px", |b| {
        b.iter(|| {
            pool.install(|| {
                tiles.par_iter().for_each(|&(tx, ty)| {
                    let mut img = ColorImage::new([0, 0], Color32::TRANSPARENT);
                    canvas.write_tile_rect_to_color_image(tx, ty, [22, 22, 42, 42], &mut img, None);
                })
            })
        });
    });
}

/// Zoomed-out stroke preview (mip level 2, 4x4 blocks) of 576 tiles with a
/// painted layer: compositing at full resolution then downsampling, versus
/// compositing and averaging in one pass.
fn bench_zoomed_out_preview(c: &mut Criterion) {
    use eframe::egui::ColorImage;
    use rayon::prelude::*;
    use rusty_painter::canvas::blend::downsample;
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let canvas = Canvas::new(4000, 4000, Color32::WHITE, 64);
    let tiles: Vec<(usize, usize)> = (20..44)
        .flat_map(|ty| (20..44).map(move |tx| (tx, ty)))
        .collect();
    // Soft, partly transparent paint in every tile.
    for &(tx, ty) in &tiles {
        let data = (0..64 * 64)
            .map(|i| {
                let a = ((i * 7 + tx * 13 + ty * 29) % 256) as u8;
                Color32::from_rgba_unmultiplied(200, 80, 40, a)
            })
            .collect();
        canvas.set_layer_tile_data(1, tx as i32, ty as i32, data);
    }
    let mut group = c.benchmark_group("zoomed_out_preview_576_tiles");
    group.bench_function("composite_then_downsample", |b| {
        b.iter(|| {
            pool.install(|| {
                tiles.par_iter().for_each(|&(tx, ty)| {
                    let mut img = ColorImage::new([0, 0], Color32::TRANSPARENT);
                    canvas.write_tile_rect_to_color_image(tx, ty, [0, 0, 64, 64], &mut img, None);
                    std::hint::black_box(downsample(&img, 2));
                })
            })
        });
    });
    group.bench_function("fused", |b| {
        b.iter(|| {
            pool.install(|| {
                tiles.par_iter().for_each(|&(tx, ty)| {
                    let mut img = ColorImage::new([0, 0], Color32::TRANSPARENT);
                    canvas.write_tile_rect_downsampled(tx, ty, [0, 0, 64, 64], 4, &mut img, None);
                    std::hint::black_box(img);
                })
            })
        });
    });
    // A gamma document (new canvases blend in gamma space): the general
    // compositor, averaging in linear light.
    let mut gamma = Canvas::new(4000, 4000, Color32::WHITE, 64);
    gamma.blend_space = rusty_painter::canvas::blend_modes::BlendSpace::Gamma;
    for &(tx, ty) in &tiles {
        let data = canvas.get_layer_tile_data(1, tx as i32, ty as i32).unwrap();
        gamma.set_layer_tile_data(1, tx as i32, ty as i32, data);
    }
    group.bench_function("fused_gamma", |b| {
        b.iter(|| {
            pool.install(|| {
                tiles.par_iter().for_each(|&(tx, ty)| {
                    let mut img = ColorImage::new([0, 0], Color32::TRANSPARENT);
                    gamma.write_tile_rect_downsampled(tx, ty, [0, 0, 64, 64], 4, &mut img, None);
                    std::hint::black_box(img);
                })
            })
        });
    });
    group.finish();
}

/// Display compositing of painted tiles through the general compositor
/// (gamma documents or non-Normal blend modes) versus the plain fast path.
fn bench_composite_modes(c: &mut Criterion) {
    use eframe::egui::ColorImage;
    use rayon::prelude::*;
    use rusty_painter::canvas::blend_modes::{BlendSpace, LayerBlend};
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let tiles: Vec<(usize, usize)> = (20..44)
        .flat_map(|ty| (20..44).map(move |tx| (tx, ty)))
        .collect();
    let painted = |space: BlendSpace, blend: LayerBlend| {
        let mut canvas = Canvas::new(4000, 4000, Color32::WHITE, 64);
        canvas.blend_space = space;
        canvas.layers[1].blend = blend;
        for &(tx, ty) in &tiles {
            let data = (0..64 * 64)
                .map(|i| Color32::from_rgba_unmultiplied(200, 80, 40, ((i * 7 + tx) % 256) as u8))
                .collect();
            canvas.set_layer_tile_data(1, tx as i32, ty as i32, data);
        }
        canvas
    };
    let mut group = c.benchmark_group("composite_modes_576_tiles");
    for (name, space, blend) in [
        (
            "linear_normal_fast_path",
            BlendSpace::Linear,
            LayerBlend::Normal,
        ),
        ("gamma_normal", BlendSpace::Gamma, LayerBlend::Normal),
        ("linear_multiply", BlendSpace::Linear, LayerBlend::Multiply),
        ("gamma_soft_light", BlendSpace::Gamma, LayerBlend::SoftLight),
    ] {
        let canvas = painted(space, blend);
        group.bench_function(name, |b| {
            b.iter(|| {
                pool.install(|| {
                    tiles.par_iter().for_each(|&(tx, ty)| {
                        let mut img = ColorImage::new([0, 0], Color32::TRANSPARENT);
                        canvas.write_tile_rect_to_color_image(
                            tx,
                            ty,
                            [0, 0, 64, 64],
                            &mut img,
                            None,
                        );
                    })
                })
            });
        });
    }
    group.finish();
}

/// Compositing with the layer features: a clipped layer, adjustment layers
/// (masked or not), against the same two plain layers.
fn bench_composite_layer_features(c: &mut Criterion) {
    use eframe::egui::ColorImage;
    use rayon::prelude::*;
    use rusty_painter::canvas::blend_modes::BlendSpace;
    use rusty_painter::canvas::filters::Filter;
    use rusty_painter::canvas::storage::LayerKind;
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let tiles: Vec<(usize, usize)> = (20..44)
        .flat_map(|ty| (20..44).map(move |tx| (tx, ty)))
        .collect();
    let paint = |canvas: &Canvas, idx: usize, rgb: (u8, u8, u8)| {
        for &(tx, ty) in &tiles {
            let data = (0..64 * 64)
                .map(|i| {
                    Color32::from_rgba_unmultiplied(rgb.0, rgb.1, rgb.2, ((i * 7 + tx) % 256) as u8)
                })
                .collect();
            canvas.set_layer_tile_data(idx, tx as i32, ty as i32, data);
        }
    };
    let two_layers = || {
        let mut canvas = Canvas::new(4000, 4000, Color32::WHITE, 64);
        canvas.blend_space = BlendSpace::Gamma;
        paint(&canvas, 1, (200, 80, 40));
        let top = canvas.insert_new_layer(2, "top".into(), LayerKind::Paint, None);
        let ti = canvas.layer_index_of(top).unwrap();
        paint(&canvas, ti, (40, 90, 200));
        (canvas, ti)
    };
    let hue = Filter::HueSaturation {
        hue: 30.0,
        saturation: 0.2,
        lightness: 0.0,
    };
    let cases: Vec<(&str, Canvas)> = vec![
        ("two_plain_layers", two_layers().0),
        ("clipped_layer", {
            let (mut canvas, ti) = two_layers();
            canvas.layers[ti].clipped = true;
            canvas
        }),
        ("adjustment_layer", {
            let (mut canvas, _) = two_layers();
            let a = canvas.insert_new_layer(3, "adj".into(), LayerKind::Paint, None);
            let ai = canvas.layer_index_of(a).unwrap();
            canvas.layers[ai].adjustment = Some(hue);
            canvas
        }),
        ("levels_adjustment_layer", {
            let (mut canvas, _) = two_layers();
            let a = canvas.insert_new_layer(3, "adj".into(), LayerKind::Paint, None);
            let ai = canvas.layer_index_of(a).unwrap();
            canvas.layers[ai].adjustment = Some(Filter::Levels {
                black: 0.1,
                white: 0.9,
                gamma: 1.3,
            });
            canvas
        }),
        ("masked_adjustment_layer", {
            let (mut canvas, _) = two_layers();
            let a = canvas.insert_new_layer(3, "adj".into(), LayerKind::Paint, None);
            let ai = canvas.layer_index_of(a).unwrap();
            canvas.layers[ai].adjustment = Some(hue);
            let m = canvas.insert_new_layer(4, "mask".into(), LayerKind::Mask { owner: a }, None);
            let mi = canvas.layer_index_of(m).unwrap();
            paint(&canvas, mi, (255, 255, 255));
            canvas
        }),
    ];
    let mut group = c.benchmark_group("composite_layer_features_576_tiles");
    for (name, canvas) in cases {
        group.bench_function(name, |b| {
            b.iter(|| {
                pool.install(|| {
                    tiles.par_iter().for_each(|&(tx, ty)| {
                        let mut img = ColorImage::new([0, 0], Color32::TRANSPARENT);
                        canvas.write_tile_rect_to_color_image(
                            tx,
                            ty,
                            [0, 0, 64, 64],
                            &mut img,
                            None,
                        );
                    })
                })
            });
        });
    }
    group.finish();
}

/// The pressure stroke with brush dynamics: each feature on its own, and
/// all together, to see what each costs over the plain stroke above.
fn bench_dynamic_strokes(c: &mut Criterion) {
    use rusty_painter::brush_engine::dynamics::{
        BrushDynamics, Randomness, SpeedDynamics, Taper, TipShape,
    };
    let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let canvas = Canvas::new(1024, 1024, Color32::WHITE, 64);
    let points: Vec<(Vec2, f32, f64)> = (0..60)
        .map(|i| {
            let t = i as f32 / 59.0;
            let pos = Vec2::new(150.0 + t * 700.0, 500.0 + (t * 9.0).sin() * 200.3);
            (
                pos,
                0.3 + 0.7 * (t * std::f32::consts::PI).sin(),
                t as f64 * 0.6,
            )
        })
        .collect();
    let taper = Taper {
        start: 80.0,
        end: 120.0,
        ..Default::default()
    };
    let tip = TipShape {
        ratio: 0.4,
        follow_stroke: true,
        random_angle: 10.0,
        ..Default::default()
    };
    let random = Randomness {
        size: 0.3,
        opacity: 0.3,
        ..Default::default()
    };
    let speed = SpeedDynamics {
        size: -0.5,
        opacity: 0.0,
    };
    let cases = [
        (
            "taper",
            BrushDynamics {
                taper,
                ..Default::default()
            },
        ),
        (
            "tip_turned_squashed",
            BrushDynamics {
                tip,
                ..Default::default()
            },
        ),
        (
            "random_size_opacity",
            BrushDynamics {
                random,
                ..Default::default()
            },
        ),
        (
            "speed",
            BrushDynamics {
                speed,
                ..Default::default()
            },
        ),
        (
            "all",
            BrushDynamics {
                tip,
                taper,
                speed,
                random,
                ..Default::default()
            },
        ),
    ];
    // A textured stroke (paper grain on every dab).
    {
        use rusty_painter::brush_engine::texture::{BrushTexture, builtin};
        let mut brush = Brush::new(60.0, 40.0, Color32::from_rgb(30, 60, 200), 10.0);
        brush.texture = Some(BrushTexture::new(builtin()[0].clone()));
        let mut group = c.benchmark_group("textured_stroke_60_samples");
        for (name, colour_random, mode) in [
            (
                "multiply",
                false,
                rusty_painter::canvas::blend_modes::LayerBlend::Multiply,
            ),
            (
                "hue_random",
                true,
                rusty_painter::canvas::blend_modes::LayerBlend::Normal,
            ),
        ] {
            let mut brush = Brush::new(60.0, 40.0, Color32::from_rgb(30, 60, 200), 10.0);
            brush.paint_blend = mode;
            if colour_random {
                brush.dynamics.random.hue = 60.0;
            }
            group.bench_function(name, |b| {
                b.iter(|| {
                    let mut undo_action = UndoAction {
                        tiles: Vec::new(),
                        selection: None,
                        transform: None,
                        layer_action: None,
                    };
                    let mut stroke_tiles = StrokeTiles::default();
                    let mut stroke = StrokeState::with_seed(1);
                    let mut context = StrokeContext::new(
                        &pool,
                        &canvas,
                        None,
                        &mut undo_action,
                        &mut stroke_tiles,
                    );
                    for &(pos, pressure, time) in &points {
                        stroke.add_sample(&mut brush, pos, pressure, Some(time), &mut context);
                    }
                    stroke.finish(&mut brush, &mut context);
                });
            });
        }
        group.bench_function("paper", |b| {
            b.iter(|| {
                let mut undo_action = UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: None,
                };
                let mut stroke_tiles = StrokeTiles::default();
                let mut stroke = StrokeState::with_seed(1);
                let mut context =
                    StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
                for &(pos, pressure, time) in &points {
                    stroke.add_sample(&mut brush, pos, pressure, Some(time), &mut context);
                }
                stroke.finish(&mut brush, &mut context);
            });
        });
        group.finish();
    }
    let mut group = c.benchmark_group("dynamic_stroke_60_samples");
    // An image tip (a 256 px speckled picture used at 60 px: mipmapped).
    {
        use rusty_painter::brush_engine::brush_options::PixelBrushShape;
        use rusty_painter::brush_engine::tip::TipMask;
        let pixels = (0..256 * 256u32)
            .map(|i| ((i.wrapping_mul(2654435761) >> 24) as u8).saturating_sub(40))
            .collect();
        let mut brush = Brush::new(60.0, 40.0, Color32::from_rgb(30, 60, 200), 10.0);
        brush.brush_options.pixel_shape =
            PixelBrushShape::Custom(TipMask::from_mask(256, 256, pixels));
        group.bench_function("image_tip", |b| {
            b.iter(|| {
                let mut undo_action = UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: None,
                };
                let mut stroke_tiles = StrokeTiles::default();
                let mut stroke = StrokeState::with_seed(1);
                let mut context =
                    StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
                for &(pos, pressure, time) in &points {
                    stroke.add_sample(&mut brush, pos, pressure, Some(time), &mut context);
                }
                stroke.finish(&mut brush, &mut context);
            });
        });
    }
    for (name, dynamics) in cases {
        let mut brush = Brush::new(
            60.0,
            40.0,
            Color32::from_rgba_unmultiplied(30, 60, 200, 255),
            10.0,
        );
        brush.dynamics = dynamics;
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut undo_action = UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: None,
                };
                let mut stroke_tiles = StrokeTiles::default();
                let mut stroke = StrokeState::with_seed(1);
                let mut context =
                    StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
                for &(pos, pressure, time) in &points {
                    stroke.add_sample(&mut brush, pos, pressure, Some(time), &mut context);
                }
                stroke.finish(&mut brush, &mut context);
            });
        });
    }
    group.finish();
}

/// Brushes built on the later features (airbrush, multiple tips…), each on
/// the same 60-sample pressure stroke as [`bench_dynamic_strokes`].
fn bench_feature_strokes(c: &mut Criterion) {
    let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let canvas = Canvas::new(1024, 1024, Color32::WHITE, 64);
    let points: Vec<(Vec2, f32, f64)> = (0..60)
        .map(|i| {
            let t = i as f32 / 59.0;
            let pos = Vec2::new(150.0 + t * 700.0, 500.0 + (t * 9.0).sin() * 200.3);
            (
                pos,
                0.3 + 0.7 * (t * std::f32::consts::PI).sin(),
                t as f64 * 0.6,
            )
        })
        .collect();
    let base = || Brush::new(60.0, 40.0, Color32::from_rgb(30, 60, 200), 10.0);
    let mut cases: Vec<(&str, Brush)> = Vec::new();
    // The airbrush, the pen resting 50 ms at every sample.
    cases.push(("airbrush", {
        let mut b = base();
        b.airbrush_rate = 60.0;
        b
    }));
    // Three image tips taken at random.
    cases.push(("three_tips", {
        use rusty_painter::brush_engine::brush_options::{PixelBrushShape, TipOrder};
        let tips: Vec<_> = rusty_painter::brush_engine::tip::builtin()
            .iter()
            .take(3)
            .map(|(_, t)| t.clone())
            .collect();
        let mut b = base();
        b.brush_options.pixel_shape = PixelBrushShape::Custom(tips[0].clone());
        b.brush_options.extra_tips = tips[1..].to_vec();
        b.brush_options.tip_order = TipOrder::Random;
        b
    }));
    // A dual brush: a spatter tip masking the round one.
    cases.push(("dual_brush", {
        use rusty_painter::brush_engine::brush_options::PixelBrushShape;
        let spatter = rusty_painter::brush_engine::tip::builtin()
            .iter()
            .find(|(n, _)| *n == "Spatter")
            .map(|(_, t)| t.clone())
            .unwrap();
        let mut b = base();
        b.dual = Some(rusty_painter::brush_engine::dual::DualTip {
            shape: PixelBrushShape::Custom(spatter),
            size: 0.5,
            ..Default::default()
        });
        b
    }));
    // Watercolour edges on a wash (applied when the pen lifts).
    cases.push(("wet_edges", {
        let mut b = base();
        b.brush_options.painting_mode =
            rusty_painter::brush_engine::brush_options::PaintingMode::Wash;
        b.brush_options.opacity = 0.6;
        b.wet_edge = 0.6;
        b
    }));
    // The new engines, at their defaults.
    for (name, t) in [
        (
            "engine_spray",
            rusty_painter::brush_engine::brush::BrushType::Spray,
        ),
        (
            "engine_chalk",
            rusty_painter::brush_engine::brush::BrushType::Chalk,
        ),
        (
            "engine_curve",
            rusty_painter::brush_engine::brush::BrushType::Curve,
        ),
        (
            "engine_grid",
            rusty_painter::brush_engine::brush::BrushType::Grid,
        ),
        (
            "engine_tangent_normal",
            rusty_painter::brush_engine::brush::BrushType::TangentNormal,
        ),
        (
            "engine_particle",
            rusty_painter::brush_engine::brush::BrushType::Particle,
        ),
    ] {
        let mut b = base();
        b.brush_type = t;
        cases.push((name, b));
    }
    // A bristle brush: 30 hairs.
    cases.push(("bristle", {
        let mut b = base();
        b.brush_type = rusty_painter::brush_engine::brush::BrushType::Bristle;
        b.bristles.count = 30;
        b
    }));
    // A ribbon, and flowers in their own colours.
    let tip = |name: &str| {
        rusty_painter::brush_engine::tip::builtin()
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| t.clone())
            .unwrap()
    };
    cases.push(("ribbon", {
        use rusty_painter::brush_engine::brush_options::{PixelBrushShape, Placement};
        let mut b = base();
        b.brush_options.pixel_shape = PixelBrushShape::Custom(tip("Striped ribbon"));
        b.brush_options.placement = Placement::Ribbon;
        b.brush_options.tip_colors = true;
        b
    }));
    cases.push(("colour_tip", {
        use rusty_painter::brush_engine::brush_options::PixelBrushShape;
        let mut b = base();
        b.brush_options.pixel_shape = PixelBrushShape::Custom(tip("Flower"));
        b.brush_options.tip_colors = true;
        b.brush_options.spacing = 60.0;
        b
    }));
    // Sketch and hatching engines.
    cases.push(("sketch", {
        let mut b = base();
        b.brush_type = rusty_painter::brush_engine::brush::BrushType::Sketch;
        b.brush_options.diameter = 3.0;
        b.sketch.density = 0.3;
        b
    }));
    cases.push(("hatching", {
        let mut b = base();
        b.brush_type = rusty_painter::brush_engine::brush::BrushType::Hatching;
        b
    }));
    // Brush inputs: four sensor → setting mappings with their curves.
    cases.push(("input_mappings", {
        use rusty_painter::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
        let mut b = base();
        let map = |sensor, setting, amount| InputMapping {
            sensor,
            setting,
            amount,
            ..Default::default()
        };
        b.inputs = vec![
            map(Sensor::Pressure, DabSetting::Size, 0.8),
            map(Sensor::Speed, DabSetting::Opacity, -0.5),
            map(Sensor::Direction, DabSetting::Angle, 0.5),
            map(Sensor::RandomStroke, DabSetting::Hue, 0.2),
        ];
        b
    }));
    // The new input targets: hardness, texture strength, scatter and the
    // secondary colour mix, on a textured brush.
    let paper = || {
        rusty_painter::brush_engine::texture::BrushTexture::new(
            rusty_painter::brush_engine::texture::builtin()[0].clone(),
        )
    };
    cases.push(("input_targets", {
        use rusty_painter::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
        let mut b = base();
        b.texture = Some(paper());
        let map = |sensor, setting, amount| InputMapping {
            sensor,
            setting,
            amount,
            ..Default::default()
        };
        b.inputs = vec![
            map(Sensor::Pressure, DabSetting::Hardness, 0.5),
            map(Sensor::Pressure, DabSetting::TextureStrength, -0.5),
            map(Sensor::RandomDab, DabSetting::Scatter, 0.2),
            map(Sensor::RandomDab, DabSetting::ColorMix, 0.5),
        ];
        b
    }));
    // Paper texture pinned (for comparison), then turned and moving with
    // the stroke, then applied to each dab.
    cases.push(("texture_pinned", {
        let mut b = base();
        b.texture = Some(paper());
        b
    }));
    cases.push(("texture_turned_following", {
        let mut b = base();
        let mut t = paper();
        t.placement.angle = 30.0;
        t.placement.follow_stroke = true;
        t.placement.random_offset = true;
        b.texture = Some(t);
        b
    }));
    cases.push(("texture_each_dab", {
        let mut b = base();
        let mut t = paper();
        t.placement.per_dab = true;
        t.placement.random_offset = true;
        b.texture = Some(t);
        b
    }));
    // Imported brushes' options: an image tip flipped at random, spacing by
    // pressure, hard edges on a soft tip, the new texture modes, and the
    // Parallel blend mode.
    cases.push(("random_flip", {
        use rusty_painter::brush_engine::brush_options::PixelBrushShape;
        let mut b = base();
        b.brush_options.pixel_shape = PixelBrushShape::Custom(tip("Spatter"));
        b.dynamics.tip.random_flip_x = true;
        b.dynamics.tip.random_flip_y = true;
        b
    }));
    cases.push(("pressure_spacing", {
        let mut b = base();
        b.brush_options.pressure_spacing = true;
        b
    }));
    cases.push(("hard_edges", {
        let mut b = base();
        b.brush_options.hardness = 0.0;
        b.sharpness = 0.4;
        b
    }));
    for (name, mode) in [
        (
            "texture_colour_dodge",
            rusty_painter::brush_engine::texture::TextureMode::ColorDodge,
        ),
        (
            "texture_hard_mix",
            rusty_painter::brush_engine::texture::TextureMode::HardMix,
        ),
    ] {
        cases.push((name, {
            let mut b = base();
            let mut t = paper();
            t.mode = mode;
            b.texture = Some(t);
            b
        }));
    }
    cases.push(("parallel_blend", {
        let mut b = base();
        b.paint_blend = rusty_painter::canvas::blend_modes::LayerBlend::Parallel;
        b
    }));
    let mut group = c.benchmark_group("feature_stroke_60_samples");
    for (name, mut brush) in cases {
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut undo_action = UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: None,
                };
                let mut stroke_tiles = StrokeTiles::default();
                let mut stroke = StrokeState::with_seed(1);
                let mut context =
                    StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
                for &(pos, pressure, time) in &points {
                    stroke.add_sample(&mut brush, pos, pressure, Some(time), &mut context);
                    stroke.airbrush(&mut brush, time + 0.05, &mut context);
                }
                stroke.finish(&mut brush, &mut context);
            });
        });
    }
    group.finish();
}

/// The same stroke with each of the round tip's extras and colour sources.
fn bench_auto_tip_strokes(c: &mut Criterion) {
    use rusty_painter::brush_engine::brush_options::{AutoTip, ColorSource};
    let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let canvas = Canvas::new(1024, 1024, Color32::WHITE, 64);
    let points: Vec<(Vec2, f32, f64)> = (0..60)
        .map(|i| {
            let t = i as f32 / 59.0;
            let pos = Vec2::new(150.0 + t * 700.0, 500.0 + (t * 9.0).sin() * 200.3);
            (
                pos,
                0.3 + 0.7 * (t * std::f32::consts::PI).sin(),
                t as f64 * 0.6,
            )
        })
        .collect();
    let tip = |t: AutoTip| move |b: &mut Brush| b.brush_options.auto_tip = t;
    let source = |s: ColorSource| move |b: &mut Brush| b.brush_options.color_source = s.clone();
    type Setup = Box<dyn Fn(&mut Brush)>;
    let cases: Vec<(&str, Setup)> = vec![
        ("plain", Box::new(|_: &mut Brush| {})),
        (
            "spikes",
            Box::new(|b: &mut Brush| {
                b.dynamics.tip.ratio = 0.4;
                b.brush_options.auto_tip.spikes = 5;
            }),
        ),
        (
            "fades",
            Box::new(tip(AutoTip {
                fade: [0.3, 0.8],
                ..Default::default()
            })),
        ),
        (
            "density_randomness",
            Box::new(tip(AutoTip {
                density: 0.7,
                randomness: 0.3,
                ..Default::default()
            })),
        ),
        (
            "uniform_random",
            Box::new(source(ColorSource::UniformRandom)),
        ),
        ("total_random", Box::new(source(ColorSource::TotalRandom))),
        (
            "pattern",
            Box::new(source(ColorSource::Pattern {
                pattern: rusty_painter::brush_engine::texture::builtin()[0].clone(),
                scale: 1.0,
            })),
        ),
    ];
    let mut group = c.benchmark_group("auto_tip_stroke_60_samples");
    for (name, set) in cases {
        let mut brush = Brush::new(60.0, 40.0, Color32::from_rgb(30, 60, 200), 10.0);
        set(&mut brush);
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut undo_action = UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: None,
                };
                let mut stroke_tiles = StrokeTiles::default();
                let mut stroke = StrokeState::with_seed(1);
                let mut context =
                    StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
                for &(pos, pressure, time) in &points {
                    stroke.add_sample(&mut brush, pos, pressure, Some(time), &mut context);
                }
                stroke.finish(&mut brush, &mut context);
            });
        });
    }
    group.finish();
}

/// Post-correction on a 300-sample shaky stroke through the stroke worker
/// (as the app paints): smoothing when the pen lifts against smoothing while
/// drawing (the stretch near the pen painted for now, again each sample).
fn bench_post_correction(c: &mut Criterion) {
    use rusty_painter::brush_engine::brush::StabilizerAlgorithm;
    use rusty_painter::brush_engine::stroke_worker::{StrokeSetup, StrokeWorker};
    use std::sync::Arc;
    let pool = Arc::new(ThreadPoolBuilder::new().num_threads(4).build().unwrap());
    let path: Vec<(Vec2, f32)> = (0..300)
        .map(|i| {
            let t = i as f32 / 299.0;
            let wobble = if i % 2 == 0 { 2.5 } else { -2.5 };
            (
                Vec2::new(40.0 + t * 900.0, 500.0 + (t * 9.0).sin() * 300.0 + wobble),
                0.3 + 0.7 * t,
            )
        })
        .collect();
    let worker = StrokeWorker::new();
    let mut group = c.benchmark_group("post_correction_300_samples");
    // Paced: each sample painted before the next comes (a pen slower than
    // the painting), so the stretch near the pen is painted for now each
    // time: the worst case.
    for (name, live, paced) in [
        ("at_pen_up", false, false),
        ("while_drawing", true, false),
        ("at_pen_up_paced", false, true),
        ("while_drawing_paced", true, true),
    ] {
        let mut brush = Brush::new(24.0, 80.0, Color32::BLACK, 10.0);
        brush.stabilizer_algorithm = StabilizerAlgorithm::PostCorrection;
        brush.stabilizer_modes.correction = 0.8;
        brush.stabilizer_modes.correction_live = live;
        group.bench_function(name, |b| {
            b.iter(|| {
                let canvas = Arc::new(Canvas::new(1024, 1024, Color32::WHITE, 64));
                worker.begin(StrokeSetup {
                    canvas,
                    brush: brush.clone(),
                    selection: None,
                    pool: Arc::clone(&pool),
                    layer_idx: 1,
                    symmetry: Default::default(),
                    view_scale: 1.0,
                    perspective: Vec::new(),
                    wrap: false,
                });
                for &(pos, pressure) in &path {
                    worker.sample(pos, pressure);
                    if paced {
                        worker.wait_idle();
                    }
                }
                worker.end();
                worker.wait_idle();
                worker.take_finished().0.len()
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_post_correction,
    bench_auto_tip_strokes,
    bench_soft_dab,
    bench_pressure_stroke,
    bench_pressure_stroke_app_pool,
    bench_dynamic_strokes,
    bench_feature_strokes,
    bench_huge_brush_stroke,
    bench_composite_dirty_tiles,
    bench_composite_damaged_rects,
    bench_zoomed_out_preview,
    bench_composite_modes,
    bench_composite_layer_features
);
criterion_main!(benches);
