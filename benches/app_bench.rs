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
    bench(
        &mut g,
        "blur_60_samples_80px",
        |a| b::set_brush(a, 80.0),
        |a| b::blend_stroke(a, &short, false),
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

criterion_group!(
    app_benches,
    painting,
    selections,
    fills,
    shapes,
    transforms,
    history_and_files
);
criterion_main!(app_benches);
