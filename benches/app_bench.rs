//! Every tool, timed end to end on a real (headless) app with a painted
//! 4000×4000 layer: the same code paths as the UI, including the stroke
//! worker and undo. Setup (painting the canvas) isn't timed.
//!
//! `cargo bench --features bench --bench app_bench`
//! (one group: `cargo bench --features bench --bench app_bench -- selection`)

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use eframe::egui::Vec2;
use rusty_painter::PainterApp;
use rusty_painter::bench_api::{
    self as b, GradientColors, GradientShape, LiquifyMode, ShapeKind, ShapeStyle, SymmetryMode,
};
use rusty_painter::selection::SelectionType;
use std::hint::black_box;
use std::time::Duration;

const N: usize = 4000;
const NF: f32 = N as f32;

fn app() -> PainterApp {
    b::painted_app(N)
}

/// Time `run` on a fresh painted app each iteration (`prepare` untimed).
fn bench(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    prepare: impl Fn(&mut PainterApp),
    run: impl Fn(&mut PainterApp),
) {
    group.bench_function(name, |bencher| {
        bencher.iter_batched(
            || {
                let mut a = app();
                prepare(&mut a);
                a
            },
            |mut a| {
                run(&mut a);
                a
            },
            BatchSize::LargeInput,
        );
    });
}

fn group<'a>(
    c: &'a mut Criterion,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.sample_size(10);
    g.measurement_time(Duration::from_secs(10));
    g
}

fn circle(center: Vec2, r: f32, n: usize) -> Vec<Vec2> {
    (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            center + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect()
}

fn painting(c: &mut Criterion) {
    let mut g = group(c, "painting");
    let path = b::wavy_path(NF, 120);
    bench(
        &mut g,
        "brush_120_samples_60px",
        |a| b::set_brush(a, 60.0),
        |a| b::stroke(a, &path, false),
    );
    bench(
        &mut g,
        "brush_120_samples_400px",
        |a| b::set_brush(a, 400.0),
        |a| b::stroke(a, &path, false),
    );
    bench(
        &mut g,
        "eraser_120_samples_120px",
        |a| b::set_brush(a, 120.0),
        |a| b::stroke(a, &path, true),
    );
    bench(
        &mut g,
        "brush_mirrored_radial_16x2_120px",
        |a| {
            b::set_brush(a, 120.0);
            b::set_symmetry(a, SymmetryMode::Radial, 16, true);
        },
        |a| b::stroke(a, &path, false),
    );
    let short = b::wavy_path(NF, 60);
    bench(
        &mut g,
        "smudge_60_samples_80px",
        |a| b::set_brush(a, 80.0),
        |a| b::blend_stroke(a, &short, true),
    );
    for (name, tip, parallel) in [
        ("mixing_brush_60_samples_80px", false, false),
        ("mixing_brush_image_tip_60_samples_80px", true, false),
        ("mixing_brush_parallel_60_samples_80px", false, true),
    ] {
        bench(
            &mut g,
            name,
            |a| {
                b::set_brush(a, 80.0);
                b::set_mixing(a, tip, parallel);
            },
            |a| b::stroke(a, &short, false),
        );
    }
    bench(
        &mut g,
        "blur_60_samples_80px",
        |a| b::set_brush(a, 80.0),
        |a| b::blend_stroke(a, &short, false),
    );
    bench(
        &mut g,
        "mixing_brush_turned_textured_60_samples_80px",
        |a| {
            b::set_brush(a, 80.0);
            b::set_mixing(a, false, false);
            b::set_turned_textured(a);
        },
        |a| b::stroke(a, &short, false),
    );
    bench(
        &mut g,
        "deform_60_samples_80px",
        |a| {
            b::set_brush(a, 80.0);
            b::set_blend_modes(a, true, false, false);
        },
        |a| b::blend_stroke(a, &short, true),
    );
    bench(
        &mut g,
        "clone_60_samples_80px",
        |a| {
            b::set_brush(a, 80.0);
            b::set_blend_modes(a, false, true, false);
        },
        |a| b::blend_stroke(a, &short, true),
    );
    bench(
        &mut g,
        "sharpen_60_samples_80px",
        |a| {
            b::set_brush(a, 80.0);
            b::set_blend_modes(a, false, false, true);
        },
        |a| b::blend_stroke(a, &short, false),
    );
    bench(
        &mut g,
        "filter_brush_blur_60_samples_80px",
        |a| {
            b::set_brush(a, 80.0);
            b::set_brush_filter(
                a,
                rusty_painter::canvas::filters::Filter::GaussianBlur { radius: 4.0 },
            );
        },
        |a| b::blend_stroke(a, &short, false),
    );
    bench(
        &mut g,
        "eyedropper_x100",
        |_| {},
        |a| {
            for i in 0..100 {
                b::eyedropper(a, Vec2::new(100.0 + i as f32 * 30.0, 2000.0));
            }
        },
    );
    g.finish();
}

fn selections(c: &mut Criterion) {
    let mut g = group(c, "selection");
    let (p0, p1) = (Vec2::new(300.0, 300.0), Vec2::new(3500.0, 3200.0));
    bench(
        &mut g,
        "rectangle_drag",
        |_| {},
        |a| b::select_drag(a, SelectionType::Rectangle, &[p0, p1]),
    );
    bench(
        &mut g,
        "ellipse_drag",
        |_| {},
        |a| {
            b::select_drag(
                a,
                SelectionType::Circle,
                &[Vec2::splat(2000.0), Vec2::new(3500.0, 2000.0)],
            )
        },
    );
    let lasso = circle(Vec2::splat(2000.0), 1500.0, 400);
    bench(
        &mut g,
        "lasso_400_points",
        |_| {},
        |a| b::select_drag(a, SelectionType::Lasso, &lasso),
    );
    let brush_path: Vec<Vec2> = (0..40)
        .map(|i| Vec2::new(500.0 + i as f32 * 70.0, 500.0 + i as f32 * 60.0))
        .collect();
    bench(
        &mut g,
        "selection_brush_40_moves_r100",
        |a| b::set_selection_brush_radius(a, 100.0),
        |a| b::select_drag(a, SelectionType::Brush, &brush_path),
    );
    // Near the origin the colours are close to the black lines, so a click
    // there floods a large area; further in, it stays in its 250 px cell.
    bench(
        &mut g,
        "magic_wand_one_cell",
        |_| {},
        |a| b::select_click(a, SelectionType::Wand, Vec2::new(1125.0, 1125.0)),
    );
    bench(
        &mut g,
        "magic_wand_large_area",
        |_| {},
        |a| b::select_click(a, SelectionType::Wand, Vec2::new(125.0, 125.0)),
    );
    bench(
        &mut g,
        "colour_range_click",
        |_| {},
        |a| b::select_click(a, SelectionType::ColorRange, Vec2::new(125.0, 125.0)),
    );
    bench(&mut g, "select_all", |_| {}, b::select_all);
    bench(
        &mut g,
        "invert_lasso_selection",
        |a| {
            b::select_drag(
                a,
                SelectionType::Lasso,
                &circle(Vec2::splat(2000.0), 1500.0, 400),
            )
        },
        b::invert_selection,
    );
    g.finish();
}

fn fills(c: &mut Criterion) {
    let mut g = group(c, "fill");
    bench(
        &mut g,
        "bucket_one_cell",
        |_| {},
        |a| b::bucket_fill(a, Vec2::new(1125.0, 1125.0), 32),
    );
    bench(
        &mut g,
        "bucket_large_area",
        |_| {},
        |a| b::bucket_fill(a, Vec2::new(125.0, 125.0), 32),
    );
    bench(
        &mut g,
        "bucket_whole_canvas",
        |_| {},
        |a| b::bucket_fill(a, Vec2::new(125.0, 125.0), 255),
    );
    let lasso = circle(Vec2::splat(2000.0), 1500.0, 200);
    bench(
        &mut g,
        "enclose_3000px",
        |_| {},
        |a| b::enclose_fill(a, &lasso),
    );
    bench(
        &mut g,
        "gradient_linear_full_canvas",
        |_| {},
        |a| {
            b::gradient(
                a,
                GradientShape::Linear,
                GradientColors::ForegroundToBackground,
                Vec2::new(200.0, 200.0),
                Vec2::new(3800.0, 3500.0),
            )
        },
    );
    bench(
        &mut g,
        "gradient_radial_to_clear",
        |_| {},
        |a| {
            b::gradient(
                a,
                GradientShape::Radial,
                GradientColors::ForegroundToTransparent,
                Vec2::splat(2000.0),
                Vec2::new(3400.0, 2600.0),
            )
        },
    );
    bench(
        &mut g,
        "smart_patch_200px",
        |a| b::select_rect(a, Vec2::new(1900.0, 1900.0), Vec2::new(2100.0, 2100.0)),
        b::content_aware_fill,
    );
    g.finish();
}

fn shapes(c: &mut Criterion) {
    let mut g = group(c, "shape");
    let (a0, a1) = (Vec2::new(500.0, 800.0), Vec2::new(3500.0, 3000.0));
    bench(
        &mut g,
        "line_40px",
        |a| b::set_brush(a, 40.0),
        |a| b::shape(a, ShapeKind::Line, ShapeStyle::Outline, a0, a1),
    );
    bench(
        &mut g,
        "rectangle_both",
        |a| b::set_brush(a, 40.0),
        |a| b::shape(a, ShapeKind::Rectangle, ShapeStyle::Both, a0, a1),
    );
    bench(
        &mut g,
        "ellipse_both",
        |a| b::set_brush(a, 40.0),
        |a| b::shape(a, ShapeKind::Ellipse, ShapeStyle::Both, a0, a1),
    );
    bench(
        &mut g,
        "ellipse_both_mirrored_4way",
        |a| {
            b::set_brush(a, 40.0);
            b::set_symmetry(a, SymmetryMode::Both, 1, false);
        },
        |a| {
            b::shape(
                a,
                ShapeKind::Ellipse,
                ShapeStyle::Both,
                Vec2::new(300.0, 300.0),
                Vec2::new(1700.0, 1500.0),
            )
        },
    );
    g.finish();
}

fn transforms(c: &mut Criterion) {
    let mut g = group(c, "transform");
    bench(
        &mut g,
        "rotate_whole_layer",
        |_| {},
        |a| b::transform_rotate(a, 0.3),
    );
    bench(
        &mut g,
        "rotate_1000px_selection",
        |a| b::select_rect(a, Vec2::new(500.0, 500.0), Vec2::new(1500.0, 1500.0)),
        |a| b::transform_rotate(a, 0.3),
    );
    let drag: Vec<Vec2> = (0..30)
        .map(|i| Vec2::new(2000.0 + i as f32 * 10.0, 2000.0))
        .collect();
    bench(
        &mut g,
        "liquify_push_30_steps_r150",
        |_| {},
        |a| b::liquify(a, LiquifyMode::Push, 150.0, &drag),
    );
    bench(
        &mut g,
        "liquify_twirl_30_steps_r150",
        |_| {},
        |a| b::liquify(a, LiquifyMode::TwirlCw, 150.0, &drag),
    );
    bench(
        &mut g,
        "palette_extract_16",
        |_| {},
        |a| {
            black_box(b::extract_palette(a, 16));
        },
    );
    bench(
        &mut g,
        "palette_recolour_dithered",
        |a| {
            b::extract_palette(a, 16);
        },
        |a| b::recolor_extracted(a, true),
    );
    g.finish();
}

fn history_and_files(c: &mut Criterion) {
    let mut g = group(c, "history_and_files");
    let full = |a: &mut PainterApp| {
        b::gradient(
            a,
            GradientShape::Linear,
            GradientColors::ForegroundToBackground,
            Vec2::ZERO,
            Vec2::new(NF, NF),
        )
    };
    bench(&mut g, "undo_full_canvas_step", full, b::undo);
    bench(
        &mut g,
        "redo_full_canvas_step",
        |a| {
            full(a);
            b::undo(a);
        },
        b::redo,
    );
    let path = b::wavy_path(NF, 120);
    bench(
        &mut g,
        "undo_brush_stroke",
        |a| {
            b::set_brush(a, 60.0);
            b::stroke(a, &path, false);
        },
        b::undo,
    );
    bench(
        &mut g,
        "save_project",
        |_| {},
        |a| {
            black_box(b::save_project(a));
        },
    );
    let bytes = b::save_project(&mut app());
    g.bench_function("open_project", |bencher| {
        bencher.iter(|| black_box(b::load_project(&bytes)))
    });
    bench(
        &mut g,
        "export_png",
        |_| {},
        |a| {
            black_box(b::export_png(a));
        },
    );
    let png = {
        let img =
            image::RgbaImage::from_fn(3000, 2000, |x, y| image::Rgba([x as u8, y as u8, 90, 255]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    bench(
        &mut g,
        "import_3000x2000_png",
        |_| {},
        |a| b::import_image(a, &png),
    );
    g.finish();
}

fn new_features(c: &mut Criterion) {
    use rusty_painter::canvas::filters::Filter;
    use rusty_painter::canvas::geometry::ImageOp;
    let mut g = group(c, "new_features");
    let hue = Filter::HueSaturation {
        hue: 30.0,
        saturation: 0.2,
        lightness: 0.0,
    };
    bench(
        &mut g,
        "filter_hue_saturation_4k",
        |_| {},
        |a| b::apply_filter(a, hue.clone()),
    );
    bench(
        &mut g,
        "filter_gaussian_blur_r8_4k",
        |_| {},
        |a| b::apply_filter(a, Filter::GaussianBlur { radius: 8.0 }),
    );
    bench(
        &mut g,
        "filter_gaussian_blur_r8_in_selection",
        |a| b::select_rect(a, Vec2::splat(1000.0), Vec2::splat(2000.0)),
        |a| b::apply_filter(a, Filter::GaussianBlur { radius: 8.0 }),
    );
    bench(
        &mut g,
        "rotate_canvas_4k",
        |_| {},
        |a| b::image_op(a, ImageOp::RotateCw),
    );
    bench(
        &mut g,
        "resize_canvas_4k_to_2k",
        |_| {},
        |a| {
            b::image_op(
                a,
                ImageOp::Resize {
                    w: 2000,
                    h: 2000,
                    smooth: true,
                },
            )
        },
    );
    bench(
        &mut g,
        "undo_canvas_rotation",
        |a| b::image_op(a, ImageOp::RotateCw),
        b::undo,
    );
    bench(
        &mut g,
        "export_psd",
        |_| {},
        |a| {
            black_box(b::psd_bytes(a));
        },
    );
    let psd = b::psd_bytes(&mut app());
    g.bench_function("open_psd", |bencher| {
        bencher.iter(|| black_box(b::open_psd(&psd)))
    });
    bench(
        &mut g,
        "timelapse_frame",
        |_| {},
        |a| {
            black_box(b::timelapse_frame(a));
        },
    );
    bench(
        &mut g,
        "text_layer_64px_paragraph",
        |_| {},
        |a| {
            b::type_text(
                a,
                "The quick brown fox\njumps over the lazy dog",
                64.0,
                Vec2::splat(500.0),
            )
        },
    );
    bench(
        &mut g,
        "composite_4k_plain",
        |_| {},
        |a| {
            black_box(b::composite_all(a));
        },
    );
    bench(
        &mut g,
        "composite_4k_with_adjustment_layer",
        |a| b::add_adjustment_layer(a, hue.clone()),
        |a| {
            black_box(b::composite_all(a));
        },
    );
    let circle: Vec<Vec2> = (0..=300)
        .map(|i| {
            let a = i as f32 / 300.0 * std::f32::consts::TAU;
            Vec2::new(
                500.0 + 300.0 * a.cos() + (i as f32 * 1.3).sin() * 4.0,
                400.0 + 200.0 * a.sin(),
            )
        })
        .collect();
    g.bench_function("quickshape_fit_300_points", |bencher| {
        bencher.iter(|| black_box(b::fit_shape(&circle)))
    });
    g.finish();
}

/// Vector layers: drawing and erasing lines over line art already there,
/// and changing every line at once. `lines` is how much is on the layer.
fn vectors(c: &mut Criterion) {
    let mut g = group(c, "vector");
    let path = b::wavy_path(NF, 120);
    let short: Vec<(Vec2, f32)> = path[50..70].to_vec();
    // `ui`: what the UI thread spends (vector layers are drawn again in the
    // background); `settled`: until the pixels are drawn too.
    for lines in [0, 500, 2000, 20000] {
        let setup = move |a: &mut PainterApp| {
            b::vector_layer(a, lines);
            b::set_brush(a, 8.0);
        };
        bench(
            &mut g,
            &format!("line_120_samples_settled/{lines}_lines"),
            setup,
            |a| b::stroke(a, &path, false),
        );
        // The same line, the pen lifting only: thinning its points, drawing
        // it again over its whole box and filing the undo step.
        bench(
            &mut g,
            &format!("line_120_samples_pen_up_ui/{lines}_lines"),
            |a| {
                setup(a);
                b::stroke_held(a, &path, false);
            },
            b::pen_up,
        );
        bench(
            &mut g,
            &format!("line_120_samples_pen_up_settled/{lines}_lines"),
            |a| {
                setup(a);
                b::stroke_held(a, &path, false);
            },
            b::finish,
        );
        bench(
            &mut g,
            &format!("short_line_20_samples_settled/{lines}_lines"),
            setup,
            |a| b::stroke(a, &short, false),
        );
    }
    for lines in [2000, 20000] {
        for (name, mode) in [
            ("whole_line", b::VectorErase::WholeLine),
            ("touched", b::VectorErase::Touched),
            ("to_crossing", b::VectorErase::ToCrossing),
        ] {
            let setup = move |a: &mut PainterApp| {
                b::vector_layer(a, lines);
                b::set_brush(a, 40.0);
                b::set_vector_erase(a, mode);
            };
            bench(
                &mut g,
                &format!("erase_{name}_120_samples_40px_ui/{lines}_lines"),
                setup,
                |a| b::stroke_unsettled(a, &path, true),
            );
            bench(
                &mut g,
                &format!("erase_{name}_120_samples_40px_settled/{lines}_lines"),
                setup,
                |a| b::stroke(a, &path, true),
            );
        }
        // A press with Edit Lines anywhere on the page: 100 of them.
        let mut a = b::painted_app(N);
        b::vector_layer(&mut a, lines);
        let presses: Vec<Vec2> = (0..100)
            .map(|i| Vec2::new((i * 397 % N) as f32, (i * 691 % N) as f32))
            .collect();
        g.bench_function(format!("pick_100_presses/{lines}_lines"), |bencher| {
            bencher.iter(|| {
                presses
                    .iter()
                    .filter_map(|&p| b::vector_pick(&a, p))
                    .count()
            })
        });
        drop(a);
        let full = move |a: &mut PainterApp| b::vector_layer(a, lines);
        bench(
            &mut g,
            &format!("recolour_ui/{lines}_lines"),
            full,
            b::recolour_vector,
        );
        bench(
            &mut g,
            &format!("recolour_settled/{lines}_lines"),
            full,
            |a| {
                b::recolour_vector(a);
                b::settle_vectors(a);
            },
        );
        bench(
            &mut g,
            &format!("line_width_x1.5_ui/{lines}_lines"),
            full,
            |a| b::vector_line_width(a, 1.5),
        );
        bench(
            &mut g,
            &format!("line_width_x1.5_settled/{lines}_lines"),
            full,
            |a| {
                b::vector_line_width(a, 1.5);
                b::settle_vectors(a);
            },
        );
        bench(
            &mut g,
            &format!("undo_recolour_ui/{lines}_lines"),
            move |a| {
                full(a);
                b::recolour_vector(a);
                b::settle_vectors(a);
            },
            b::undo,
        );
        bench(
            &mut g,
            &format!("rasterise/{lines}_lines"),
            full,
            b::rasterise_vector,
        );
    }
    // Shapes: filled, outlined, dashed with arrowheads, gradients.
    let shapes = |a: &mut PainterApp| b::vector_shapes(a, 2000);
    bench(
        &mut g,
        "draw_whole_canvas/2000_shapes",
        shapes,
        b::redraw_vectors,
    );
    bench(&mut g, "line_width_x1.5_settled/2000_shapes", shapes, |a| {
        b::vector_line_width(a, 1.5);
        b::settle_vectors(a);
    });
    let svg = b::svg_document(10_000, N);
    bench(
        &mut g,
        "import_svg/10000_paths",
        |_| {},
        |a| b::import_svg(a, &svg),
    );
    g.finish();
}

/// Vector layers, the rest: Edit Vectors on many items at once (box pick,
/// flip, align, spread, combine, snap), its correcting brushes, the
/// Transform tool keeping lines lines, the sharp picture drawn zoomed in,
/// and saving, opening and SVG export.
fn vector_editing(c: &mut Criterion) {
    let mut g = group(c, "vector_edit");
    let mid = |a: f32, b: f32| (Vec2::splat(NF * a), Vec2::splat(NF * b));
    for lines in [2000, 20000] {
        let setup = move |a: &mut PainterApp| b::vector_layer(a, lines);
        // A 1000 px box in the middle (about 1/16 of the lines touch it).
        let (lo, hi) = mid(0.375, 0.625);
        let mut a = b::painted_app(N);
        setup(&mut a);
        for (name, inside) in [("touching", false), ("inside", true)] {
            let n = b::vector_box_pick(&mut a, lo, hi, inside);
            g.bench_function(
                format!("box_pick_{name}_{n}_picked/{lines}_lines"),
                |bencher| bencher.iter(|| black_box(b::vector_box_pick(&mut a, lo, hi, inside))),
            );
        }
        let points: Vec<Vec2> = (0..1000)
            .map(|i| Vec2::new((i * 397 % N) as f32, (i * 691 % N) as f32))
            .collect();
        g.bench_function(format!("snap_1000_points_12px/{lines}_lines"), |bencher| {
            bencher.iter(|| black_box(b::snap_points(&a, &points, 12.0)))
        });
        // The sharp picture: a 1920×1080 screen zoomed in on the middle,
        // and a stroke's 256 px patch of it.
        for scale in [2.0, 4.0] {
            g.bench_function(
                format!("sharp_screen_1080p_x{scale}/{lines}_lines"),
                |bencher| {
                    bencher.iter(|| black_box(b::sharp_picture(&mut a, lo, 1920, 1080, scale)))
                },
            );
        }
        g.bench_function(format!("sharp_patch_256px_x4/{lines}_lines"), |bencher| {
            bencher.iter(|| black_box(b::sharp_picture(&mut a, lo, 256, 256, 4.0)))
        });
        g.bench_function(format!("svg_export/{lines}_lines"), |bencher| {
            bencher.iter(|| black_box(b::vector_svg(&a).len()))
        });
        drop(a);
        let picked = move |a: &mut PainterApp| {
            setup(a);
            b::vector_box_pick(a, lo, hi, false);
        };
        bench(
            &mut g,
            &format!("flip_picked_settled/{lines}_lines"),
            picked,
            b::flip_picked,
        );
        bench(
            &mut g,
            &format!("align_left_picked_settled/{lines}_lines"),
            picked,
            b::align_picked_left,
        );
        bench(
            &mut g,
            &format!("distribute_picked_settled/{lines}_lines"),
            picked,
            b::distribute_picked,
        );
        bench(
            &mut g,
            &format!("transform_rotate_settled/{lines}_lines"),
            setup,
            |a| {
                b::transform_rotate(a, 0.3);
                b::settle_vectors(a);
            },
        );
        // Redraw: a line drawn again 1 px lower, end to end (a 4 px
        // brush, so it's that line its ends are on).
        bench(
            &mut g,
            &format!("correct_redraw_a_line_settled/{lines}_lines"),
            setup,
            |a| {
                let along = b::along_line(a, 1.0);
                b::correct(a, b::CorrectMode::Redraw, 4.0, &along)
            },
        );
        // Correcting brushes: a 120-sample drag across the middle, 40 px.
        let drag: Vec<Vec2> = b::wavy_path(NF, 120).into_iter().map(|(p, _)| p).collect();
        for (name, mode) in [
            ("simplify", b::CorrectMode::Simplify),
            ("connect", b::CorrectMode::Connect),
            ("pinch", b::CorrectMode::Pinch),
            ("width", b::CorrectMode::Width),
            ("pen_width", b::CorrectMode::PenWidth),
        ] {
            let drag = drag.clone();
            bench(
                &mut g,
                &format!("correct_{name}_120_samples_40px_settled/{lines}_lines"),
                setup,
                move |a| b::correct(a, mode, 40.0, &drag),
            );
        }
    }
    // Combining overlapping squares picked all at once.
    for n in [16, 100, 400] {
        let setup = move |a: &mut PainterApp| {
            b::vector_squares(a, n);
            b::vector_box_pick(a, Vec2::ZERO, Vec2::splat(NF), false);
        };
        bench(
            &mut g,
            &format!("combine_union_settled/{n}_squares"),
            setup,
            |a| b::combine_picked(a, true),
        );
        bench(
            &mut g,
            &format!("combine_exclude_settled/{n}_squares"),
            setup,
            |a| b::combine_picked(a, false),
        );
    }
    // Shapes: drawn sharp, exported, transformed.
    let mut a = b::painted_app(N);
    b::vector_shapes(&mut a, 2000);
    let (lo, _) = mid(0.375, 0.625);
    g.bench_function("sharp_screen_1080p_x4/2000_shapes", |bencher| {
        bencher.iter(|| black_box(b::sharp_picture(&mut a, lo, 1920, 1080, 4.0)))
    });
    g.bench_function("svg_export/2000_shapes", |bencher| {
        bencher.iter(|| black_box(b::vector_svg(&a).len()))
    });
    drop(a);
    bench(
        &mut g,
        "transform_rotate_settled/2000_shapes",
        |a| b::vector_shapes(a, 2000),
        |a| {
            b::transform_rotate(a, 0.3);
            b::settle_vectors(a);
        },
    );
    // Project files with a vector layer of 20000 lines.
    let mut a = b::painted_app(N);
    b::vector_layer(&mut a, 20000);
    let bytes = b::save_project(&mut a);
    g.bench_function("save_project/20000_lines", |bencher| {
        bencher.iter(|| black_box(b::save_project(&mut a).len()))
    });
    g.bench_function("open_project/20000_lines", |bencher| {
        bencher.iter(|| black_box(b::load_project(&bytes)))
    });
    g.finish();
}

criterion_group!(
    app_benches,
    vectors,
    vector_editing,
    painting,
    selections,
    fills,
    shapes,
    transforms,
    history_and_files,
    new_features
);
criterion_main!(app_benches);
