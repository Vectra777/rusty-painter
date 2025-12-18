use criterion::{black_box, Criterion, criterion_group, criterion_main, BenchmarkId, Throughput};
use eframe::egui::Color32;
use rusty_painter::canvas::canvas::{alpha_over, alpha_over_batch, alpha_over_simd_x4};

fn bench_alpha_over_scalar(c: &mut Criterion) {
    let src = Color32::from_rgba_unmultiplied(255, 128, 64, 200);
    let dst = Color32::from_rgba_unmultiplied(64, 128, 255, 150);
    
    c.bench_function("alpha_over_scalar", |b| {
        b.iter(|| {
            black_box(alpha_over(black_box(src), black_box(dst)))
        })
    });
}

fn bench_alpha_over_simd_x4(c: &mut Criterion) {
    let src = [
        Color32::from_rgba_unmultiplied(255, 128, 64, 200),
        Color32::from_rgba_unmultiplied(200, 100, 50, 180),
        Color32::from_rgba_unmultiplied(180, 90, 45, 160),
        Color32::from_rgba_unmultiplied(160, 80, 40, 140),
    ];
    let dst = [
        Color32::from_rgba_unmultiplied(64, 128, 255, 150),
        Color32::from_rgba_unmultiplied(50, 100, 200, 130),
        Color32::from_rgba_unmultiplied(45, 90, 180, 120),
        Color32::from_rgba_unmultiplied(40, 80, 160, 110),
    ];
    
    c.bench_function("alpha_over_simd_x4", |b| {
        b.iter(|| {
            black_box(alpha_over_simd_x4(black_box(src), black_box(dst)))
        })
    });
}

fn bench_alpha_over_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("alpha_over_batch");
    
    for size in [64, 256, 1024, 4096].iter() {
        let src: Vec<Color32> = (0..*size)
            .map(|i| Color32::from_rgba_unmultiplied(
                (i % 256) as u8,
                ((i * 2) % 256) as u8,
                ((i * 3) % 256) as u8,
                200,
            ))
            .collect();
        let dst: Vec<Color32> = (0..*size)
            .map(|i| Color32::from_rgba_unmultiplied(
                ((i * 3) % 256) as u8,
                ((i * 2) % 256) as u8,
                (i % 256) as u8,
                150,
            ))
            .collect();
        let mut out = vec![Color32::TRANSPARENT; *size];
        
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("simd", size), size, |b, _| {
            b.iter(|| {
                alpha_over_batch(
                    black_box(&src),
                    black_box(&dst),
                    black_box(&mut out),
                )
            })
        });
        
        // Scalar baseline for comparison
        group.bench_with_input(BenchmarkId::new("scalar", size), size, |b, _| {
            b.iter(|| {
                for i in 0..*size {
                    out[i] = alpha_over(black_box(src[i]), black_box(dst[i]));
                }
            })
        });
    }
    
    group.finish();
}

fn bench_tile_merge(c: &mut Criterion) {
    // Simulate merging a full 64x64 tile (4096 pixels)
    let tile_size = 64 * 64;
    let src: Vec<Color32> = (0..tile_size)
        .map(|i| Color32::from_rgba_unmultiplied(
            (i % 256) as u8,
            ((i / 64) % 256) as u8,
            128,
            180,
        ))
        .collect();
    let dst: Vec<Color32> = vec![Color32::from_rgba_unmultiplied(255, 255, 255, 255); tile_size];
    let mut out = vec![Color32::TRANSPARENT; tile_size];
    
    let mut group = c.benchmark_group("tile_merge");
    group.throughput(Throughput::Elements(tile_size as u64));
    
    group.bench_function("simd", |b| {
        b.iter(|| {
            alpha_over_batch(
                black_box(&src),
                black_box(&dst),
                black_box(&mut out),
            )
        })
    });
    
    group.bench_function("scalar", |b| {
        b.iter(|| {
            for i in 0..tile_size {
                out[i] = alpha_over(black_box(src[i]), black_box(dst[i]));
            }
        })
    });
    
    group.finish();
}

criterion_group!(
    benches,
    bench_alpha_over_scalar,
    bench_alpha_over_simd_x4,
    bench_alpha_over_batch,
    bench_tile_merge
);
criterion_main!(benches);
