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
    let mut context =
        StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut stroke_tiles);
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
    let mut brush = Brush::new(60.0, 40.0, Color32::from_rgba_unmultiplied(30, 60, 200, 255), 10.0);
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

/// A 1500 px soft brush dragged across a 4000 px canvas: the case Krita's
/// "Instant Preview" exists for.
fn bench_huge_brush_stroke(c: &mut Criterion) {
    let pool = ThreadPoolBuilder::new().build().unwrap();
    let canvas = Canvas::new(4000, 4000, Color32::WHITE, 64);
    let mut brush = Brush::new(1500.0, 40.0, Color32::from_rgba_unmultiplied(200, 60, 30, 255), 10.0);
    let points: Vec<Vec2> = (0..40)
        .map(|i| Vec2::new(250.0 + i as f32 * 90.0, 2000.0 + (i as f32 * 0.2).sin() * 600.0))
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
    let tiles: Vec<(usize, usize)> = (20..44).flat_map(|ty| (20..44).map(move |tx| (tx, ty))).collect();
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

criterion_group!(
    benches,
    bench_soft_dab,
    bench_pressure_stroke,
    bench_huge_brush_stroke,
    bench_composite_dirty_tiles
);
criterion_main!(benches);
