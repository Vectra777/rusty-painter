//! Fill, gradient and filled shapes on a big canvas (8000² by default),
//! timed end to end through the app's tool code (undo push included):
//!
//!     cargo run --release --features bench --example big_canvas_tools [size] [op|all]
//!
//! `op` runs just one of them (for `perf record`). With `CHECK=1`, digests
//! of the painted and the undone layer follow (to compare two builds).
use eframe::egui::Vec2;
use rusty_painter::bench_api::{self as b, GradientColors, GradientShape, ShapeKind, ShapeStyle};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8000);
    let only = args.next().filter(|o| o != "all");
    // Digests of the result and of the undone layer (to compare builds).
    let check = std::env::var_os("CHECK").is_some();
    let f = n as f32;
    type Op = (
        &'static str,
        bool,
        Box<dyn Fn(&mut rusty_painter::PainterApp)>,
    );
    let ops: Vec<Op> = vec![
        (
            "bucket_blank_whole",
            false,
            Box::new(|a| b::bucket_fill(a, Vec2::splat(10.0), 32)),
        ),
        (
            "bucket_painted_cell",
            true,
            Box::new(|a| b::bucket_fill(a, Vec2::splat(1125.0), 32)),
        ),
        (
            "bucket_painted_tol255",
            true,
            Box::new(|a| b::bucket_fill(a, Vec2::splat(125.0), 255)),
        ),
        (
            "gradient_linear_full",
            false,
            Box::new(move |a| {
                b::gradient(
                    a,
                    GradientShape::Linear,
                    GradientColors::ForegroundToBackground,
                    Vec2::splat(0.05 * f),
                    Vec2::splat(0.95 * f),
                )
            }),
        ),
        (
            "rect_filled",
            false,
            Box::new(move |a| {
                b::shape(
                    a,
                    ShapeKind::Rectangle,
                    ShapeStyle::Fill,
                    Vec2::splat(0.05 * f),
                    Vec2::splat(0.95 * f),
                )
            }),
        ),
        (
            "ellipse_filled",
            false,
            Box::new(move |a| {
                b::shape(
                    a,
                    ShapeKind::Ellipse,
                    ShapeStyle::Fill,
                    Vec2::splat(0.05 * f),
                    Vec2::splat(0.95 * f),
                )
            }),
        ),
        (
            "ellipse_both",
            false,
            Box::new(move |a| {
                b::set_brush(a, 40.0);
                b::shape(
                    a,
                    ShapeKind::Ellipse,
                    ShapeStyle::Both,
                    Vec2::splat(0.05 * f),
                    Vec2::splat(0.95 * f),
                )
            }),
        ),
    ];
    for (name, painted, op) in &ops {
        if only.as_deref().is_some_and(|o| o != *name) {
            continue;
        }
        let mut app = if *painted {
            b::painted_app(n)
        } else {
            b::blank_app(n, n)
        };
        let t = Instant::now();
        op(&mut app);
        b::finish(&mut app);
        let run = t.elapsed();
        // Every tile composited again at full size (what a redraw of the
        // changed area costs before it goes to the GPU).
        let t = Instant::now();
        b::composite_all(&app);
        let redraw = t.elapsed();
        let digest = if check { b::layer_digest(&app) } else { 0 };
        let t = Instant::now();
        b::undo(&mut app);
        let undo = t.elapsed();
        let undone = if check { b::layer_digest(&app) } else { 0 };
        println!(
            "{name:24} {:8.1} ms   undo {:7.1} ms   redraw {:7.1} ms   {digest:016x} {undone:016x}",
            run.as_secs_f64() * 1e3,
            undo.as_secs_f64() * 1e3,
            redraw.as_secs_f64() * 1e3
        );
    }
}
