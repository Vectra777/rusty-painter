use super::*;

fn pool() -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap()
}

#[test]
fn box_blur_keeps_flat_areas_and_softens_edges() {
    let side = 9;
    let flat = vec![[10.0, 20.0, 30.0, 255.0]; side * side];
    let out = box_blur(&pool(), &flat, side, 2);
    assert!(
        out.iter()
            .all(|p| (p[0] - 10.0).abs() < 1e-3 && (p[3] - 255.0).abs() < 1e-3)
    );
    // A hard vertical edge becomes a ramp.
    let edge: Vec<[f32; 4]> = (0..side * side)
        .map(|i| if i % side < 4 { [0.0; 4] } else { [1.0; 4] })
        .collect();
    let out = box_blur(&pool(), &edge, side, 2);
    let mid = out[4 * side + 4][0];
    assert!(mid > 0.08 && mid < 0.92, "{mid}");
}

/// The blur as it was, one line at a time, columns read across memory.
fn box_blur_reference(src: &[[f32; 4]], side: usize, r: usize) -> Vec<[f32; 4]> {
    let pass = |input: &[[f32; 4]], horizontal: bool| -> Vec<[f32; 4]> {
        let mut out = vec![[0.0; 4]; side * side];
        for line in 0..side {
            let at = |i: usize| {
                if horizontal {
                    line * side + i
                } else {
                    i * side + line
                }
            };
            let col: Vec<[f32; 4]> = (0..side).map(|i| input[at(i)]).collect();
            let mut res = vec![[0.0; 4]; side];
            blur_line(&col, &mut res, r, 0);
            for (i, v) in res.into_iter().enumerate() {
                out[at(i)] = v;
            }
        }
        out
    };
    let mut v = src.to_vec();
    for _ in 0..2 {
        v = pass(&v, true);
        v = pass(&v, false);
    }
    v
}

#[test]
fn the_parallel_box_blur_matches_the_line_by_line_one() {
    let mut seed = 7u32;
    let mut rand = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1 << 24) as f32
    };
    // Small (one thread) and big (bands in parallel), radius small and
    // past the patch.
    for (side, r) in [(9, 2), (40, 6), (101, 7), (201, 30), (130, 200)] {
        let src: Vec<[f32; 4]> = (0..side * side)
            .map(|_| [rand(), rand(), rand(), rand()])
            .collect();
        let fast = box_blur(&pool(), &src, side, r);
        let slow = box_blur_reference(&src, side, r);
        let worst = fast
            .iter()
            .zip(&slow)
            .flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs()))
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "side {side} r {r}: off by {worst}");
    }
}

#[test]
fn the_box_blur_inside_a_margin_is_the_whole_one_cut_down() {
    let side = 41;
    let src: Vec<[f32; 4]> = (0..side * side)
        .map(|i| {
            let v = ((i * 7919) % 101) as f32 / 100.0;
            [v, 1.0 - v, v * 0.5, 1.0]
        })
        .collect();
    for (r, pad) in [(1, 1), (3, 3), (6, 6), (4, 10)] {
        let whole = box_blur(&pool(), &src, side, r);
        let inside = box_blur_inside(&pool(), &src, side, r, pad);
        let inner = side - 2 * pad;
        for y in 0..inner {
            for x in 0..inner {
                let (a, b) = (inside[y * inner + x], whole[(y + pad) * side + x + pad]);
                for c in 0..4 {
                    assert!((a[c] - b[c]).abs() < 1e-5, "r {r} pad {pad} ({x}, {y})");
                }
            }
        }
    }
}
