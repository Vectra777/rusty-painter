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

/// A 1500 px soft brush dragged across a 4000 px canvas: the case Krita's
/// "Instant Preview" exists for.
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

criterion_group!(
    benches,
    bench_soft_dab,
    bench_pressure_stroke,
    bench_pressure_stroke_app_pool,
    bench_dynamic_strokes,
    bench_feature_strokes,
    bench_huge_brush_stroke,
    bench_composite_dirty_tiles,
    bench_composite_damaged_rects,
    bench_zoomed_out_preview,
    bench_composite_modes
);
criterion_main!(benches);
