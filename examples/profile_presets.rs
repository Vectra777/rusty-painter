//! Every built-in brush preset painting the same stroke through the real
//! app (stroke worker, undo, wet paint drying between strokes), its
//! samples coming as a pen's do, laggiest first: how long the paint goes
//! on after the pen stops. With a name, that
//! preset alone, over and over (to record it with `perf`/`flamegraph`).
//!
//!     cargo run --release --features bench --example profile_presets
//!     cargo run --release --features bench --example profile_presets -- "Wet Round" 20 [size]
use rusty_painter::bench_api as api;
use std::time::Instant;

const SIZE: usize = 2048;
const SAMPLES: usize = 120;

/// A pen's samples come this often (seconds): about 200 a second.
const PEN: f64 = 0.005;

/// One stroke with `brush` on a fresh painted canvas, its samples coming
/// as a pen's do: how long the paint goes on after the pen stops (the
/// lag), and a second of wet paint drying after it (if it paints wet).
fn run(brush: &rusty_painter::brush_engine::brush::Brush) -> (f64, Option<f64>) {
    let mut app = api::painted_app(SIZE);
    api::use_brush(&mut app, brush.clone());
    let path = api::wavy_path(SIZE as f32, SAMPLES);
    let start = Instant::now();
    api::stroke_paced(&mut app, &path, |i| {
        let due = start + std::time::Duration::from_secs_f64(i as f64 * PEN);
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
    });
    let t = Instant::now();
    api::finish(&mut app);
    let stroke = t.elapsed().as_secs_f64();
    let t = Instant::now();
    // 30 steps of 1/30 s, as the UI runs them (a few a frame).
    let wet = api::dry(&mut app, 30).then(|| t.elapsed().as_secs_f64());
    (stroke, wet)
}

fn main() {
    let presets = api::default_presets();
    let mut args = std::env::args().skip(1);
    if let Some(name) = args.next() {
        let rounds: usize = args.next().map_or(10, |n| n.parse().unwrap());
        let (_, mut brush) = presets
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("no preset {name}"))
            .clone();
        // A third argument: another size.
        if let Some(size) = args.next() {
            brush.brush_options.diameter = size.parse().unwrap();
        }
        for _ in 0..rounds {
            let (stroke, wet) = run(&brush);
            println!(
                "{name}: lag {:.1} ms, drying {:?}",
                stroke * 1e3,
                wet.map(|w| w * 1e3)
            );
        }
        return;
    }
    let mut rows: Vec<(String, f32, f64, Option<f64>)> = presets
        .iter()
        .map(|(name, brush)| {
            run(brush); // warm up (tips, textures, the pool)
            let (stroke, wet) = run(brush);
            (name.clone(), brush.brush_options.diameter, stroke, wet)
        })
        .collect();
    rows.sort_by(|a, b| (b.2 + b.3.unwrap_or(0.0)).total_cmp(&(a.2 + a.3.unwrap_or(0.0))));
    println!(
        "{:<32} {:>7} {:>9} {:>12}",
        "preset", "size", "lag ms", "drying ms"
    );
    for (name, size, stroke, wet) in rows {
        println!(
            "{:<32} {:>7.0} {:>9.1} {:>12}",
            name,
            size,
            stroke * 1e3,
            wet.map_or(String::new(), |w| format!("{:.1}", w * 1e3)),
        );
    }
}
