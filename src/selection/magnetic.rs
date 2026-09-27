//! Magnetic lasso: a live wire that snaps to edges ("intelligent
//! scissors", Mortensen & Barrett).
//!
//! Stepping onto a pixel costs less the stronger the edge there (Sobel
//! gradient of the luminance), so the cheapest path between two points runs
//! along edges. [`LiveWire`] builds the shortest-path tree from one anchor
//! over a window around it once; following the cursor is then only a walk
//! back up the tree.

use crate::canvas::fill::Reference;
use eframe::egui::Vec2;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Cost of stepping onto a pixel with no edge, relative to a strong edge
/// (which costs [`EDGE_FLOOR`]): how strongly the wire prefers edges.
const EDGE_FLOOR: f32 = 0.08;

/// Per-pixel step cost over a window of the canvas.
struct EdgeMap {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    cost: Vec<f32>,
}

impl EdgeMap {
    fn compute(reference: &dyn Reference, [x0, y0, x1, y1]: [i32; 4]) -> Self {
        let (w, h) = ((x1 - x0).max(1) as usize, (y1 - y0).max(1) as usize);
        let pixels = reference.render(x0, y0, w, h);
        // Luminance of each pixel shown over white (transparent reads as
        // paper), so edges of paint on an empty layer still count.
        let lum: Vec<f32> = pixels
            .iter()
            .map(|c| {
                let over = |v: u8| (v as f32 + (255 - c.a()) as f32) / 255.0;
                0.2126 * over(c.r()) + 0.7152 * over(c.g()) + 0.0722 * over(c.b())
            })
            .collect();
        let at = |x: isize, y: isize| {
            let x = x.clamp(0, w as isize - 1) as usize;
            let y = y.clamp(0, h as isize - 1) as usize;
            lum[y * w + x]
        };
        let mut grad = vec![0.0f32; w * h];
        let mut max = 0.0f32;
        for y in 0..h as isize {
            for x in 0..w as isize {
                let gx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1))
                    - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
                let gy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1))
                    - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
                let g = (gx * gx + gy * gy).sqrt();
                grad[y as usize * w + x as usize] = g;
                max = max.max(g);
            }
        }
        // Relative to the strongest edge nearby, but a faint texture in a
        // flat area doesn't become a "strong" edge.
        let scale = 1.0 / max.max(0.25);
        let cost = grad
            .iter()
            .map(|g| EDGE_FLOOR + (1.0 - EDGE_FLOOR) * (1.0 - (g * scale).min(1.0)))
            .collect();
        Self { x0, y0, w, h, cost }
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let (lx, ly) = (x - self.x0, y - self.y0);
        (lx >= 0 && ly >= 0 && (lx as usize) < self.w && (ly as usize) < self.h)
            .then(|| ly as usize * self.w + lx as usize)
    }
}

/// Shortest paths from one anchor to every pixel of a window around it.
pub struct LiveWire {
    map: EdgeMap,
    seed: usize,
    /// Previous pixel on the cheapest path from the seed (itself at the seed).
    parent: Vec<u32>,
}

impl LiveWire {
    /// Paths from `anchor` over `window` (`[x0, y0, x1, y1)`, clipped by the
    /// caller to the canvas and containing the anchor).
    pub fn new(reference: &dyn Reference, anchor: Vec2, window: [i32; 4]) -> Self {
        let map = EdgeMap::compute(reference, window);
        let (ax, ay) = (anchor.x.floor() as i32, anchor.y.floor() as i32);
        let seed = map
            .index(ax, ay)
            .unwrap_or_else(|| map.index(map.x0, map.y0).unwrap_or(0));
        let n = map.w * map.h;
        let mut dist = vec![f32::INFINITY; n];
        let mut parent = vec![u32::MAX; n];
        let mut heap = BinaryHeap::new();
        dist[seed] = 0.0;
        parent[seed] = seed as u32;
        // Costs are non-negative, so their bit patterns order like the values.
        heap.push(Reverse((0.0f32.to_bits(), seed as u32)));
        const STEPS: [(isize, isize, f32); 8] = [
            (1, 0, 1.0),
            (-1, 0, 1.0),
            (0, 1, 1.0),
            (0, -1, 1.0),
            (1, 1, std::f32::consts::SQRT_2),
            (1, -1, std::f32::consts::SQRT_2),
            (-1, 1, std::f32::consts::SQRT_2),
            (-1, -1, std::f32::consts::SQRT_2),
        ];
        let (w, h) = (map.w as isize, map.h as isize);
        while let Some(Reverse((d, i))) = heap.pop() {
            let d = f32::from_bits(d);
            let i = i as usize;
            if d > dist[i] {
                continue;
            }
            let (x, y) = ((i % map.w) as isize, (i / map.w) as isize);
            for (dx, dy, len) in STEPS {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let j = ny as usize * map.w + nx as usize;
                let nd = d + map.cost[j] * len;
                if nd < dist[j] {
                    dist[j] = nd;
                    parent[j] = i as u32;
                    heap.push(Reverse((nd.to_bits(), j as u32)));
                }
            }
        }
        Self { map, seed, parent }
    }

    /// The window the paths cover.
    pub fn window(&self) -> [i32; 4] {
        let m = &self.map;
        [m.x0, m.y0, m.x0 + m.w as i32, m.y0 + m.h as i32]
    }

    /// Whether `p` is far enough inside the window to trace a path to it.
    pub fn reaches(&self, p: Vec2) -> bool {
        let [x0, y0, x1, y1] = self.window();
        let (x, y) = (p.x.floor() as i32, p.y.floor() as i32);
        x > x0 && y > y0 && x < x1 - 1 && y < y1 - 1
    }

    /// The cheapest path from the anchor to `p`, as pixel centres from the
    /// anchor, simplified to within half a pixel. `None` outside the window.
    pub fn path_to(&self, p: Vec2) -> Option<Vec<Vec2>> {
        let mut i = self.map.index(p.x.floor() as i32, p.y.floor() as i32)?;
        let mut path = Vec::new();
        let w = self.map.w;
        loop {
            path.push(Vec2::new(
                (self.map.x0 + (i % w) as i32) as f32 + 0.5,
                (self.map.y0 + (i / w) as i32) as f32 + 0.5,
            ));
            if i == self.seed {
                break;
            }
            let next = self.parent[i] as usize;
            if next == i || next >= self.parent.len() {
                break;
            }
            i = next;
        }
        path.reverse();
        Some(simplify(&path, 0.5))
    }
}

/// A window around `a` and `b` with `margin` pixels to spare, clipped to the
/// `width`×`height` canvas.
pub fn window_around(a: Vec2, b: Vec2, margin: f32, width: usize, height: usize) -> [i32; 4] {
    let min = a.min(b) - Vec2::splat(margin);
    let max = a.max(b) + Vec2::splat(margin);
    [
        (min.x.floor() as i32).clamp(0, width as i32),
        (min.y.floor() as i32).clamp(0, height as i32),
        (max.x.ceil() as i32).clamp(0, width as i32),
        (max.y.ceil() as i32).clamp(0, height as i32),
    ]
}

/// Ramer–Douglas–Peucker: drop points within `epsilon` of the line through
/// their neighbours.
pub fn simplify(points: &[Vec2], epsilon: f32) -> Vec<Vec2> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (pa, pb) = (points[a], points[b]);
        let d = pb - pa;
        let len = d.length();
        let mut worst = (0.0f32, a);
        for (i, &p) in points.iter().enumerate().take(b).skip(a + 1) {
            let dist = if len > 0.0 {
                (d.x * (pa.y - p.y) - d.y * (pa.x - p.x)).abs() / len
            } else {
                (p - pa).length()
            };
            if dist > worst.0 {
                worst = (dist, i);
            }
        }
        if worst.0 > epsilon {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

/// Length of a polyline.
pub fn path_length(points: &[Vec2]) -> f32 {
    points.windows(2).map(|w| (w[1] - w[0]).length()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    /// A white 64×64 image with a vertical black line at x = 32.
    fn line_image() -> impl Fn(i32, i32, usize, usize) -> Vec<Color32> + Sync {
        move |x, y, w, h| {
            let mut out = Vec::with_capacity(w * h);
            for _yy in y..y + h as i32 {
                for xx in x..x + w as i32 {
                    out.push(if xx == 32 {
                        Color32::BLACK
                    } else {
                        Color32::WHITE
                    });
                }
            }
            out
        }
    }

    #[test]
    fn the_wire_follows_an_edge() {
        let reference = line_image();
        // From well right of the line at the top to well left of it at the
        // bottom: a straight path would cross the line diagonally; the wire
        // runs to it and hugs it on the way down.
        let a = Vec2::new(44.5, 4.5);
        let b = Vec2::new(20.5, 60.5);
        let wire = LiveWire::new(&reference, a, window_around(a, b, 16.0, 64, 64));
        let path = wire.path_to(b).unwrap();
        assert_eq!(path.first().copied(), Some(a));
        assert_eq!(path.last().copied(), Some(b));
        // Where the path crosses a few heights down the middle.
        let x_at = |y: f32| {
            path.windows(2)
                .find(|s| s[0].y <= y && s[1].y >= y)
                .map(|s| {
                    let t = if s[1].y > s[0].y {
                        (y - s[0].y) / (s[1].y - s[0].y)
                    } else {
                        0.0
                    };
                    s[0].x + (s[1].x - s[0].x) * t
                })
        };
        let hugging = [20.0, 32.0, 44.0]
            .iter()
            .all(|&y| x_at(y).is_some_and(|x| (x - 32.5).abs() <= 1.5));
        assert!(hugging, "path strays from the edge: {path:?}");
    }

    #[test]
    fn simplify_drops_collinear_points() {
        let pts: Vec<Vec2> = (0..10).map(|i| Vec2::new(i as f32, 0.0)).collect();
        assert_eq!(simplify(&pts, 0.5), vec![pts[0], pts[9]]);
    }

    #[test]
    #[ignore = "timing; run with --release --ignored"]
    fn live_wire_largest_window() {
        // The biggest window auto-anchoring allows: a span of 320 px plus
        // margins on both sides, over a noisy image.
        let reference = |x: i32, y: i32, w: usize, h: usize| {
            let mut out = Vec::with_capacity(w * h);
            for yy in y..y + h as i32 {
                for xx in x..x + w as i32 {
                    let v = ((xx * 7919 + yy * 104729) % 251) as u8;
                    out.push(Color32::from_gray(v));
                }
            }
            out
        };
        let (a, b) = (Vec2::new(200.0, 200.0), Vec2::new(520.0, 520.0));
        let t = std::time::Instant::now();
        let wire = LiveWire::new(&reference, a, window_around(a, b, 160.0, 4096, 4096));
        let built = t.elapsed();
        let t = std::time::Instant::now();
        let path = wire.path_to(b).unwrap();
        eprintln!(
            "build {built:?}, trace {:?} ({} points)",
            t.elapsed(),
            path.len()
        );
    }
}
