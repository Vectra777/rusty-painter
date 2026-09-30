//! Benchmarks for the painting tools: mirror painting, shapes, selections,
//! gradients and the smart patch. Sizes are realistic worst-ish cases (big
//! brushes, full 4096 px canvases).
//!
//! Run with `cargo bench --bench tools_bench`; compare against a saved run
//! with `cargo bench --bench tools_bench -- --save-baseline before` then
//! `-- --baseline before`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;
use rusty_painter::brush_engine::brush::Brush;
use rusty_painter::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use rusty_painter::brush_engine::symmetry::{Symmetry, SymmetryMode};
use rusty_painter::canvas::fill::{self, ColorMatch, FillSettings};
use rusty_painter::canvas::gradient::{Gradient, GradientRepeat, GradientShape, Ramp};
use rusty_painter::canvas::history::UndoAction;
use rusty_painter::canvas::inpaint::{self, Problem};
use rusty_painter::canvas::{Canvas, blend_modes::BlendSpace};
use rusty_painter::selection::magnetic::{LiveWire, window_around};
use rusty_painter::selection::{
    SelectionManager, SelectionMask, SelectionMode, SelectionShape, new_lasso_shape,
};
use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn undo() -> UndoAction {
    UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    }
}

/// Something the optimizer can't drop: how many tiles the stroke touched.
fn u_len(tiles: &StrokeTiles) -> usize {
    tiles.dirty.len()
}

fn pool() -> rayon::ThreadPool {
    ThreadPoolBuilder::new().build().unwrap()
}

/// A textured 4096² picture: diagonal bands with some noise, the kind of
/// content the fills and selections look at.
fn picture(size: usize) -> Vec<Color32> {
    (0..size * size)
        .map(|i| {
            let (x, y) = (i % size, i / size);
            let band = ((x + y) / 37) % 2;
            let v = (band * 150 + (x * 7 + y * 13) % 40) as u8;
            Color32::from_rgb(v, v / 2 + 40, 200 - v / 2)
        })
        .collect()
}

fn reference(
    px: &[Color32],
    size: usize,
) -> impl Fn(i32, i32, usize, usize) -> Vec<Color32> + Sync + '_ {
    move |x, y, w, h| {
        let mut out = Vec::with_capacity(w * h);
        for yy in y as usize..y as usize + h {
            out.extend_from_slice(&px[yy * size + x as usize..yy * size + x as usize + w]);
        }
        out
    }
}

/// A curvy stroke of `n` samples across a `size` canvas.
fn stroke_path(size: f32, n: usize) -> Vec<(Vec2, f32)> {
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1) as f32;
            let p = Vec2::new(
                size * (0.15 + 0.3 * t),
                size * (0.2 + 0.25 * (t * 7.0).sin() * 0.5 + 0.2 * t),
            );
            (p, 0.4 + 0.6 * t)
        })
        .collect()
}

// --- Mirror painting --------------------------------------------------------

fn bench_symmetry(c: &mut Criterion) {
    let pool = pool();
    let size = 2048;
    let path = stroke_path(size as f32, 80);
    let modes = [
        ("off", SymmetryMode::Off, 1, false),
        ("vertical", SymmetryMode::Vertical, 1, false),
        ("four_way", SymmetryMode::Both, 1, false),
        ("radial_8", SymmetryMode::Radial, 8, false),
        ("radial_16_mirrored", SymmetryMode::Radial, 16, true),
        ("radial_32_mirrored", SymmetryMode::Radial, 32, true),
    ];
    let mut group = c.benchmark_group("mirror_stroke_80_samples_120px");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));
    for (name, mode, count, mirrored) in modes {
        let symmetry = Symmetry {
            mode,
            center: Vec2::splat(size as f32 * 0.5),
            angle: 0.0,
            count,
            mirrored,
        };
        let copies = symmetry.copies();
        group.bench_function(name, |b| {
            b.iter(|| {
                let canvas = Canvas::new(size, size, Color32::WHITE, 64);
                let mut brush = Brush::new(120.0, 40.0, Color32::from_rgb(30, 60, 200), 15.0);
                let (mut undo, mut tiles, mut stroke) =
                    (undo(), StrokeTiles::default(), StrokeState::new());
                let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles)
                    .with_symmetry(&symmetry, &copies);
                for &(p, pressure) in &path {
                    stroke.add_point(&mut brush, p, pressure, &mut ctx);
                }
                black_box(u_len(&tiles));
            });
        });
    }
    group.finish();
}

// --- Shapes -----------------------------------------------------------------

fn ellipse(center: Vec2, r: Vec2) -> Vec<Vec2> {
    let n = 4096;
    (0..=n)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            center + Vec2::new(a.cos() * r.x, a.sin() * r.y)
        })
        .collect()
}

fn bench_shapes(c: &mut Criterion) {
    let pool = pool();
    let size = 4096;
    let mut group = c.benchmark_group("shape");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));
    let outline = ellipse(Vec2::splat(2048.0), Vec2::new(1500.0, 1100.0));
    // The fill: the ellipse rasterized (anti-aliased) and painted.
    group.bench_function("fill_3000px_ellipse", |b| {
        b.iter(|| {
            let canvas = Canvas::new(size, size, Color32::WHITE, 64);
            let mut manager = SelectionManager::new();
            manager.canvas_size = [size, size];
            pool.install(|| {
                manager.apply_shape(new_lasso_shape(outline.clone()), SelectionMode::Add)
            });
            let Some(SelectionShape::Mask(mask)) = &manager.current_shape else {
                panic!("no mask");
            };
            let mut u = undo();
            pool.install(|| canvas.paint_mask(1, mask, Color32::from_rgb(200, 80, 40), &mut u));
            black_box(u.tiles.len());
        });
    });
    // The outline: the ellipse's perimeter stroked with a 40 px brush.
    group.bench_function("outline_3000px_ellipse_40px", |b| {
        b.iter(|| {
            let canvas = Canvas::new(size, size, Color32::WHITE, 64);
            let mut brush = Brush::new(40.0, 60.0, Color32::BLACK, 15.0);
            let (mut u, mut tiles, mut stroke) =
                (undo(), StrokeTiles::default(), StrokeState::new());
            let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut u, &mut tiles);
            for &p in &outline {
                stroke.add_point(&mut brush, p, 1.0, &mut ctx);
            }
            black_box(u_len(&tiles));
        });
    });
    group.finish();
}

// --- Selections -------------------------------------------------------------

fn bench_selection(c: &mut Criterion) {
    let pool = pool();
    let size = 4096;
    let px = picture(size);
    let r = reference(&px, size);
    let mut group = c.benchmark_group("selection");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(6));

    group.bench_function("magic_wand_4k", |b| {
        let settings = FillSettings::default();
        b.iter(|| {
            pool.install(|| black_box(fill::bucket_fill(&r, size, size, (10, 10), &settings)))
        });
    });
    group.bench_function("magic_wand_4k_close_gaps_8", |b| {
        let settings = FillSettings {
            gap: 8,
            ..FillSettings::default()
        };
        b.iter(|| {
            pool.install(|| black_box(fill::bucket_fill(&r, size, size, (10, 10), &settings)))
        });
    });
    for (name, matching) in [
        (
            "colour_range_4k_channels",
            ColorMatch::Channels { tolerance: 24 },
        ),
        (
            "colour_range_4k_perceptual",
            ColorMatch::Perceptual {
                tolerance: 8.0,
                softness: 6.0,
            },
        ),
    ] {
        group.bench_function(name, |b| {
            let target = px[10];
            b.iter(|| {
                pool.install(|| black_box(fill::select_color(&r, size, size, target, matching)))
            });
        });
    }

    // A big soft selection to grow, shrink, invert and combine.
    let mut manager = SelectionManager::new();
    manager.canvas_size = [size, size];
    manager.apply_shape(
        SelectionShape::Circle {
            center: Vec2::splat(2048.0),
            radius: 1500.0,
        },
        SelectionMode::Add,
    );
    let Some(SelectionShape::Mask(circle)) = manager.current_shape.clone() else {
        panic!("no mask");
    };
    for radius in [10, -10, 40] {
        group.bench_with_input(
            BenchmarkId::new("grow_3000px_circle", radius),
            &radius,
            |b, &radius| {
                b.iter(|| pool.install(|| black_box(circle.grown(radius))));
            },
        );
    }
    group.bench_function("invert_4k", |b| {
        b.iter(|| {
            let mut m = SelectionManager::new();
            m.canvas_size = [size, size];
            m.current_shape = Some(SelectionShape::Mask(circle.clone()));
            pool.install(|| m.invert());
            black_box(m.current_shape.is_some());
        });
    });
    let square = SelectionMask::new(500, 500, 3000, 3000, vec![255; 3000 * 3000]);
    for mode in [
        SelectionMode::Add,
        SelectionMode::Subtract,
        SelectionMode::Intersect,
    ] {
        group.bench_with_input(
            BenchmarkId::new("combine_3000px", format!("{mode:?}")),
            &mode,
            |b, &mode| {
                b.iter(|| pool.install(|| black_box(circle.combine(&square, mode))));
            },
        );
    }
    group.bench_function("magnetic_wire_build_640px", |b| {
        let (a, e) = (Vec2::new(900.0, 900.0), Vec2::new(1220.0, 1220.0));
        b.iter(|| black_box(LiveWire::new(&r, a, window_around(a, e, 160.0, size, size))));
    });
    group.bench_function("magnetic_wire_trace", |b| {
        let (a, e) = (Vec2::new(900.0, 900.0), Vec2::new(1220.0, 1220.0));
        let wire = LiveWire::new(&r, a, window_around(a, e, 160.0, size, size));
        b.iter(|| black_box(wire.path_to(e)));
    });
    group.finish();
}

// --- Gradients --------------------------------------------------------------

fn bench_gradient(c: &mut Criterion) {
    let pool = pool();
    let size = 4096;
    let canvas = Canvas::new(size, size, Color32::WHITE, 64);
    let region = pool.install(|| canvas.capture_region(1, [0, 0, size as i32, size as i32]));
    let mut group = c.benchmark_group("gradient");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(6));
    group.bench_function("ramp_build", |b| {
        b.iter(|| {
            black_box(Ramp::new(
                Color32::from_rgb(240, 120, 30),
                Color32::from_rgb(40, 30, 120),
                BlendSpace::Linear,
                1.0,
            ))
        });
    });
    for shape in [
        GradientShape::Linear,
        GradientShape::Radial,
        GradientShape::Angle,
    ] {
        let ramp = Ramp::new(
            Color32::from_rgb(240, 120, 30),
            Color32::from_rgb(40, 30, 120),
            BlendSpace::Linear,
            1.0,
        );
        let gradient = Gradient {
            shape,
            repeat: GradientRepeat::None,
            start: Vec2::new(500.0, 700.0),
            end: Vec2::new(3500.0, 3000.0),
            reverse: false,
        };
        group.bench_function(BenchmarkId::new("repaint_4k", format!("{shape:?}")), |b| {
            b.iter(|| {
                let row = |x0: i32, y: i32, out: &mut [Color32]| {
                    let mut t = vec![0.0; out.len()];
                    gradient.row_positions(x0, y, &mut t);
                    for (i, (o, &t)) in out.iter_mut().zip(&t).enumerate() {
                        let n = rusty_painter::canvas::blend_modes::pixel_noise(
                            (x0 + i as i32) as u32,
                            y as u32,
                        );
                        *o = ramp.pixel(t, Some(n));
                    }
                };
                pool.install(|| canvas.paint_over_region(1, &region, row));
            });
        });
    }
    group.bench_function("capture_region_4k", |b| {
        b.iter(|| {
            black_box(pool.install(|| canvas.capture_region(1, [0, 0, size as i32, size as i32])))
        });
    });
    group.finish();
}

// --- Smart patch ------------------------------------------------------------

fn bench_inpaint(c: &mut Criterion) {
    let pool = pool();
    let mut group = c.benchmark_group("smart_patch");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for hole in [64usize, 200, 400] {
        // The crop the app gives a hole: the hole plus a margin its size.
        let side = hole * 3;
        let px: Vec<[f32; 4]> = (0..side * side)
            .map(|i| {
                let (x, y) = (i % side, i / side);
                let v = ((x * 7 + y * 13) % 23) as f32 / 23.0 * 0.5
                    + ((x / 9 + y / 11) % 2) as f32 * 0.4;
                [v, v * 0.8, v * 0.6, 1.0]
            })
            .collect();
        let problem = Problem {
            w: side,
            h: side,
            pixels: px,
            hole: (0..side * side)
                .map(|i| {
                    let (x, y) = (i % side, i / side);
                    (hole..2 * hole).contains(&x) && (hole..2 * hole).contains(&y)
                })
                .collect(),
        };
        group.bench_with_input(
            BenchmarkId::new("fill_square_hole", hole),
            &problem,
            |b, problem| {
                b.iter(|| {
                    pool.install(|| {
                        black_box(inpaint::inpaint(problem, &AtomicBool::new(false), &|_| {}))
                    })
                });
            },
        );
    }
    group.finish();
}

// --- Filters, image operations, text ---------------------------------------

fn bench_filters(c: &mut Criterion) {
    use rusty_painter::canvas::filters::Filter;
    let pool = pool();
    let size = 1024;
    let px = picture(size);
    let mut group = c.benchmark_group("filter_1024px");
    for (name, f) in [
        (
            "hue_saturation",
            Filter::HueSaturation {
                hue: 30.0,
                saturation: 0.2,
                lightness: 0.0,
            },
        ),
        (
            "levels",
            Filter::Levels {
                black: 0.1,
                white: 0.9,
                gamma: 1.2,
            },
        ),
        ("gaussian_r4", Filter::GaussianBlur { radius: 4.0 }),
        ("gaussian_r30", Filter::GaussianBlur { radius: 30.0 }),
        (
            "motion_40px",
            Filter::MotionBlur {
                angle: 30.0,
                distance: 40.0,
            },
        ),
        (
            "sharpen",
            Filter::Sharpen {
                radius: 2.0,
                amount: 1.0,
            },
        ),
        (
            "noise",
            Filter::Noise {
                amount: 0.2,
                mono: false,
                size: 1.0,
            },
        ),
        ("pixelate_16", Filter::Pixelate { size: 16 }),
        (
            "line_art",
            Filter::LineArt {
                black: 0.25,
                white: 0.85,
                keep_color: false,
            },
        ),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| pool.install(|| black_box(f.apply(&px, size, size, (0, 0)))))
        });
    }
    group.finish();
}

fn bench_image_ops(c: &mut Criterion) {
    use rusty_painter::canvas::geometry::ImageOp;
    let size = 2048;
    let px = picture(size);
    let mut group = c.benchmark_group("image_op_2048px_layer");
    for (name, op) in [
        ("rotate_cw", ImageOp::RotateCw),
        ("flip_horizontal", ImageOp::FlipHorizontal),
        (
            "crop_to_half",
            ImageOp::Reframe {
                x: 512,
                y: 512,
                w: 1024,
                h: 1024,
            },
        ),
        (
            "resize_half_smooth",
            ImageOp::Resize {
                w: 1024,
                h: 1024,
                smooth: true,
            },
        ),
        (
            "resize_double_hard",
            ImageOp::Resize {
                w: 4096,
                h: 4096,
                smooth: false,
            },
        ),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| black_box(op.apply(&px, size, size, Color32::TRANSPARENT)))
        });
    }
    group.finish();
}

fn bench_text(c: &mut Criterion) {
    use rusty_painter::canvas::text::{self, TextStyle};
    let font = text::builtin_fonts().remove(0).1;
    let style = TextStyle {
        size: 64.0,
        ..Default::default()
    };
    let paragraph = "The quick brown fox jumps\nover the lazy dog, twice\nand then once more.";
    let mut group = c.benchmark_group("text");
    group.bench_function("render_3_lines_64px", |b| {
        b.iter(|| black_box(text::render(&font, paragraph, &style, Vec2::splat(100.0))))
    });
    group.finish();
}

criterion_group!(
    tools,
    bench_symmetry,
    bench_shapes,
    bench_selection,
    bench_gradient,
    bench_inpaint,
    bench_filters,
    bench_image_ops,
    bench_text
);
criterion_main!(tools);
