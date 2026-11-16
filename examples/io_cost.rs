//! Save, PNG export, the document copy a background save works on, undo,
//! and an alpha-locked stroke over a mostly empty layer, timed through the
//! app's own code:
//!
//!     cargo run --release --features bench --example io_cost [size]
use rusty_painter::bench_api as b;
use std::time::Instant;

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    println!("{label:<28} {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
    out
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096);
    let f = n as f32;

    // Four painted layers.
    let mut app = b::painted_app(n);
    for _ in 0..3 {
        b::duplicate_layer(&mut app);
    }
    time("detached_copy (4 layers)", || b::detached_copy(&app));
    // A working session's undo history: 40 broad strokes.
    b::set_brush(&mut app, 250.0);
    time("40 strokes", || {
        for _ in 0..40 {
            b::stroke(&mut app, &b::wavy_path(f, 400), false);
        }
    });
    println!("{:<28} {:>8}", "  undo steps", b::undo_depth(&app));
    time("undo x10 + redo x10", || {
        (0..10).for_each(|_| b::undo(&mut app));
        (0..10).for_each(|_| b::redo(&mut app));
    });
    let bytes = time("save_project", || b::save_project(&mut app));
    println!(
        "{:<28} {:>8.1} MB",
        "  project size",
        bytes.len() as f64 / 1e6
    );
    let png = time("export_png", || b::export_png(&mut app));
    println!("{:<28} {:>8.1} MB", "  png size", png.len() as f64 / 1e6);
    time("load_project", || b::load_project(&bytes));

    // An outlined layer under the one being painted: each redraw of the
    // whole canvas made its outline again, now only the first.
    let mut app = b::painted_app(n);
    b::duplicate_layer(&mut app);
    time("redraw, plain layers", || b::composite_all(&app));
    let mut app = b::painted_app(n);
    b::set_border(&mut app, 12.0);
    b::duplicate_layer(&mut app);
    time("redraw, outlined layer (cold)", || b::composite_all(&app));
    time("redraw, outlined layer (warm)", || b::composite_all(&app));
    b::set_brush(&mut app, 120.0);
    b::stroke(&mut app, &b::wavy_path(f, 400), false);
    time("redraw after a stroke above", || b::composite_all(&app));

    // A small blob on an empty layer, then a big alpha-locked stroke
    // across the whole canvas: almost every tile it crosses is empty.
    let mut app = b::blank_app(n, n);
    b::set_brush(&mut app, 40.0);
    b::stroke(
        &mut app,
        &[(eframe::egui::Vec2::splat(50.0), 1.0); 2],
        false,
    );
    b::set_alpha_lock(&mut app, true);
    b::set_brush(&mut app, 300.0);
    let path = b::wavy_path(f, 2000);
    time("alpha_locked_stroke", || b::stroke(&mut app, &path, false));
}
