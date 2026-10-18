//! The README's brush showcase: one stroke from each of a few built-in
//! presets on a blank canvas, its name under it, saved as a PNG.
//!
//!     cargo run --release --features bench --example brush_showcase -- docs/src/brushes.png
use eframe::egui::{Color32, Vec2};
use rusty_painter::bench_api as api;

const PRESETS: [&str; 24] = [
    "Pencil (Sketch)",
    "Ink Pen",
    "Calligraphy",
    "Hard Round",
    "Soft Airbrush",
    "Multiply Marker",
    "Chalk",
    "Chalk Grainy",
    "Dry Bristles",
    "Oil Bristle",
    "Watercolour",
    "Watercolour Wash",
    "Wet Round",
    "Drippy Ink",
    "Spray",
    "Charcoal Pencil Medium",
    "Cross Hatch",
    "Screentone Pressure",
    "Foliage",
    "Flowers",
    "Stitches",
    "Lace Ribbon",
    "Bristles-5 Flat",
    "Stamp Sparkles",
];
const COLS: usize = 4;
const CELL: (f32, f32) = (500.0, 210.0);

fn main() {
    let out = std::env::args().nth(1).unwrap_or("brushes.png".into());
    let rows = PRESETS.len().div_ceil(COLS);
    let mut app = api::blank_app(COLS * CELL.0 as usize, rows * CELL.1 as usize);
    let presets = api::default_presets();
    let palette = [(32, 46, 92), (196, 64, 52), (34, 120, 104), (214, 150, 40)];
    for (i, name) in PRESETS.iter().enumerate() {
        let (_, mut brush) = presets
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no preset {name}"))
            .clone();
        // A size that fits the cell, whatever the preset's own.
        let d = &mut brush.brush_options.diameter;
        *d = d.clamp(4.0, 48.0);
        let (r, g, b) = palette[i % palette.len()];
        brush.brush_options.color = Color32::from_rgb(r, g, b);
        api::use_brush(&mut app, brush);
        let origin = Vec2::new((i % COLS) as f32 * CELL.0, (i / COLS) as f32 * CELL.1);
        // An S-curve across the cell, pressure rising then easing off.
        let path: Vec<(Vec2, f32)> = (0..160)
            .map(|k| {
                let t = k as f32 / 159.0;
                let p = Vec2::new(
                    60.0 + 380.0 * t,
                    85.0 - 45.0 * (t * std::f32::consts::TAU).sin(),
                );
                (origin + p, 0.25 + 0.75 * (t * std::f32::consts::PI).sin())
            })
            .collect();
        api::stroke(&mut app, &path, false);
        api::dry(&mut app, 60);
        let mut ink = presets[0].1.clone();
        ink.brush_options.color = Color32::from_gray(60);
        api::use_brush(&mut app, ink);
        api::type_text(&mut app, name, 26.0, origin + Vec2::new(60.0, 160.0));
    }
    std::fs::write(&out, api::export_png(&mut app)).expect("write the PNG");
    println!("{out}");
}
