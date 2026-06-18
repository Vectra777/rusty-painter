use criterion::{Criterion, criterion_group, criterion_main};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;
use rusty_painter::{
    brush_engine::{
        brush::Brush,
        stroke::{StrokeContext, StrokeState},
    },
    canvas::{Canvas, history::UndoAction},
};
use std::collections::HashSet;

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
    };
    let mut modified_tiles = HashSet::new();

    // Warm up the mask cache and tile allocation so the measurement focuses on per-dab work.
    let mut stroke = StrokeState::new();
    let mut context =
        StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut modified_tiles);
    stroke.add_point(&mut brush, Vec2 { x: 256.0, y: 256.0 }, &mut context);
    undo_action.tiles.clear();
    modified_tiles.clear();

    c.bench_function("soft_dab_512px", |b| {
        b.iter(|| {
            let mut stroke = StrokeState::new();
            undo_action.tiles.clear();
            modified_tiles.clear();

            let mut context =
                StrokeContext::new(&pool, &canvas, None, &mut undo_action, &mut modified_tiles);
            stroke.add_point(&mut brush, Vec2 { x: 256.0, y: 256.0 }, &mut context);
            stroke.add_point(&mut brush, Vec2 { x: 280.0, y: 256.0 }, &mut context);
        });
    });
}

criterion_group!(benches, bench_soft_dab);
criterion_main!(benches);
