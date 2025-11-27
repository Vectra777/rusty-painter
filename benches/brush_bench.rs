use criterion::{criterion_group, criterion_main, Criterion};
use eframe::egui::Color32;
use rayon::ThreadPoolBuilder;
use rusty_painter::{
    brush_engine::brush::{Brush, StrokeState},
    canvas::{canvas::Canvas, history::UndoAction},
    utils::{color::Color, vector::Vec2},
};
use std::collections::HashSet;

fn bench_soft_dab(c: &mut Criterion) {
    let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let canvas = Canvas::new(512, 512, Color32::WHITE, 64);
    let mut brush = Brush::new(48.0, 50.0, Color::rgba(0, 0, 0, 255), 20.0);
    let mut undo_action = UndoAction { tiles: Vec::new() };
    let mut modified_tiles = HashSet::new();

    // Warm up the mask cache and tile allocation so the measurement focuses on per-dab work.
    let mut stroke = StrokeState::new();
    stroke.add_point(
        &pool,
        &canvas,
        &mut brush,
        Vec2 { x: 256.0, y: 256.0 },
        &mut undo_action,
        &mut modified_tiles,
    );
    undo_action.tiles.clear();
    modified_tiles.clear();

    c.bench_function("soft_dab_512px", |b| {
        b.iter(|| {
            let mut stroke = StrokeState::new();
            undo_action.tiles.clear();
            modified_tiles.clear();

            stroke.add_point(
                &pool,
                &canvas,
                &mut brush,
                Vec2 { x: 256.0, y: 256.0 },
                &mut undo_action,
                &mut modified_tiles,
            );
            stroke.add_point(
                &pool,
                &canvas,
                &mut brush,
                Vec2 { x: 280.0, y: 256.0 },
                &mut undo_action,
                &mut modified_tiles,
            );
        });
    });
}

criterion_group!(benches, bench_soft_dab);
criterion_main!(benches);
