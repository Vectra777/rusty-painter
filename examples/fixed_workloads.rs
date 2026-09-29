//! Fixed workloads on one thread, for counting instructions with `perf
//! stat` (steady where wall-clock timings drift with the machine's state):
//!
//!     cargo build --release --example fixed_workloads
//!     perf stat -e instructions:u target/release/examples/fixed_workloads plain_stroke
//!
//! Compare two builds by running the same workload in each.
use eframe::egui::{Color32, ColorImage, Vec2};
use rayon::ThreadPoolBuilder;
use rusty_painter::brush_engine::brush::Brush;
use rusty_painter::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use rusty_painter::canvas::blend_modes::{BlendSpace, LayerBlend};
use rusty_painter::canvas::{Canvas, history::UndoAction};

fn stroke(brush: &mut Brush, rounds: usize) {
    let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let canvas = Canvas::new(1024, 1024, Color32::WHITE, 64);
    for _ in 0..rounds {
        let mut undo = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let mut tiles = StrokeTiles::default();
        let mut state = StrokeState::with_seed(1);
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        for i in 0..60 {
            let t = i as f32 / 59.0;
            let pos = Vec2::new(150.0 + t * 700.0, 500.0 + (t * 9.0).sin() * 200.3);
            let pressure = 0.3 + 0.7 * (t * std::f32::consts::PI).sin();
            state.add_sample(brush, pos, pressure, Some(t as f64 * 0.6), &mut ctx);
            state.airbrush(brush, t as f64 * 0.6 + 0.05, &mut ctx);
        }
        state.finish(brush, &mut ctx);
    }
}

/// 576 painted tiles of a 4000 px canvas, composited (`block` > 1: the
/// zoomed-out preview) `rounds` times.
fn composite(space: BlendSpace, blend: LayerBlend, block: usize, rounds: usize) {
    let mut canvas = Canvas::new(4000, 4000, Color32::WHITE, 64);
    canvas.blend_space = space;
    canvas.layers[1].blend = blend;
    let tiles: Vec<(usize, usize)> = (20..44)
        .flat_map(|ty| (20..44).map(move |tx| (tx, ty)))
        .collect();
    for &(tx, ty) in &tiles {
        let data = (0..64 * 64)
            .map(|i| Color32::from_rgba_unmultiplied(200, 80, 40, ((i * 7 + tx) % 256) as u8))
            .collect();
        canvas.set_layer_tile_data(1, tx as i32, ty as i32, data);
    }
    let mut img = ColorImage::new([0, 0], Color32::TRANSPARENT);
    for _ in 0..rounds {
        for &(tx, ty) in &tiles {
            if block > 1 {
                canvas.write_tile_rect_downsampled(tx, ty, [0, 0, 64, 64], block, &mut img, None);
            } else {
                canvas.write_tile_rect_to_color_image(tx, ty, [0, 0, 64, 64], &mut img, None);
            }
            std::hint::black_box(&img);
        }
    }
}

fn main() {
    let workload = std::env::args().nth(1).unwrap_or_default();
    let base = || Brush::new(60.0, 40.0, Color32::from_rgb(30, 60, 200), 10.0);
    match workload.as_str() {
        "plain_stroke" => stroke(&mut base(), 20),
        "airbrush_stroke" => stroke(
            &mut {
                let mut b = base();
                b.airbrush_rate = 60.0;
                b
            },
            10,
        ),
        "paper_stroke" => stroke(
            &mut {
                let mut b = base();
                let paper = rusty_painter::brush_engine::texture::builtin()[0].clone();
                b.texture = Some(rusty_painter::brush_engine::texture::BrushTexture::new(
                    paper,
                ));
                b
            },
            10,
        ),
        "placed_paper_stroke" => stroke(
            &mut {
                let mut b = base();
                let paper = rusty_painter::brush_engine::texture::builtin()[0].clone();
                let mut t = rusty_painter::brush_engine::texture::BrushTexture::new(paper);
                t.placement.angle = 30.0;
                t.placement.follow_stroke = true;
                b.texture = Some(t);
                b
            },
            10,
        ),
        "composite_linear_fast" => composite(BlendSpace::Linear, LayerBlend::Normal, 1, 20),
        "composite_gamma_normal" => composite(BlendSpace::Gamma, LayerBlend::Normal, 1, 20),
        "composite_gamma_soft_light" => composite(BlendSpace::Gamma, LayerBlend::SoftLight, 1, 5),
        "preview_linear" => composite(BlendSpace::Linear, LayerBlend::Normal, 4, 20),
        "preview_gamma" => composite(BlendSpace::Gamma, LayerBlend::Normal, 4, 5),
        other => {
            eprintln!(
                "unknown workload {other:?}: plain_stroke, airbrush_stroke, paper_stroke, \
                 placed_paper_stroke, composite_linear_fast, composite_gamma_normal, \
                 composite_gamma_soft_light, preview_linear, preview_gamma"
            );
            std::process::exit(2);
        }
    }
}
