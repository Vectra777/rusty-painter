//! Wall and CPU time of a 60-sample pressure stroke on an app-sized thread
//! pool (every logical core). CPU time counts every thread, so it shows work
//! wasted on thread hand-offs and spinning that wall time hides; use it to
//! tune `PIXELS_PER_THREAD` in `brush_engine/dab.rs`.
//!
//!     cargo run --release --example stroke_cost -- 60    # brush diameter
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;
use rusty_painter::brush_engine::{
    brush::Brush,
    stroke::{StrokeContext, StrokeState, StrokeTiles},
};
use rusty_painter::canvas::{Canvas, history::UndoAction};

fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let f: Vec<&str> = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

fn main() {
    let diameter: f32 = std::env::args().nth(1).map_or(60.0, |a| a.parse().unwrap());
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let canvas = Canvas::new(2048, 2048, Color32::WHITE, 64);
    let mut brush = Brush::new(
        diameter,
        40.0,
        Color32::from_rgba_unmultiplied(30, 60, 200, 255),
        10.0,
    );
    let points: Vec<(Vec2, f32)> = (0..60)
        .map(|i| {
            let t = i as f32 / 59.0;
            (
                Vec2::new(150.0 + t * 1500.0, 1000.0 + (t * 9.0).sin() * 400.3),
                0.3 + 0.7 * (t * std::f32::consts::PI).sin(),
            )
        })
        .collect();
    let run = |n: usize, brush: &mut Brush| {
        for _ in 0..n {
            let mut undo = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut tiles = StrokeTiles::default();
            let mut stroke = StrokeState::new();
            let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
            for &(p, pr) in &points {
                stroke.add_point(brush, p, pr, &mut ctx);
            }
        }
    };
    run(20, &mut brush);
    let (c0, t0) = (cpu_seconds(), std::time::Instant::now());
    let n = if diameter > 300.0 { 6 } else { 150 };
    run(n, &mut brush);
    let wall = t0.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - c0;
    println!(
        "d={diameter:>5} wall/stroke {:7.3} ms   cpu/stroke {:7.3} ms   cpu/wall {:4.1} cores",
        wall * 1e3 / n as f64,
        cpu * 1e3 / n as f64,
        cpu / wall
    );
}
