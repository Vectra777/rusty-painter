//! Drawing a bent mesh: each triangle filled from the picture where it lay
//! at rest. Pixels are taken at their middles with a strict fill rule (a
//! pixel on an edge two triangles share is drawn once), and the mesh's edge
//! never has paint on it ([`super::mesh`] leaves a ring), so no edge needs
//! smoothing. Where the mesh folds over itself, later triangles go over
//! earlier ones.

use super::mesh::PuppetMesh;
use eframe::egui::Color32;
use rayon::prelude::*;

/// The pixels a puppet is made of (premultiplied), placed on the canvas.
#[derive(Clone, Debug, Default)]
pub struct Picture {
    pub x0: i32,
    pub y0: i32,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Color32>,
}

impl Picture {
    /// Bilinear, premultiplied, at canvas point `(x, y)` (transparent
    /// outside).
    fn sample(&self, x: f32, y: f32) -> [f32; 4] {
        let (x, y) = (x - 0.5 - self.x0 as f32, y - 0.5 - self.y0 as f32);
        let (ix, iy) = (floor(x), floor(y));
        let (fx, fy) = (x - ix, y - iy);
        let (ix, iy) = (ix as i32, iy as i32);
        let (w, h) = (self.width as i32, self.height as i32);
        let at = |px: i32, py: i32| -> [f32; 4] {
            if px < 0 || py < 0 || px >= w || py >= h {
                return [0.0; 4];
            }
            let c = self.pixels[(py * w + px) as usize];
            [c.r() as f32, c.g() as f32, c.b() as f32, c.a() as f32]
        };
        if fx == 0.0 && fy == 0.0 {
            return at(ix, iy);
        }
        if ix >= 0 && iy >= 0 && ix + 1 < w && iy + 1 < h {
            // All four inside: no edge checks.
            let i = (iy * w + ix) as usize;
            let get = |i: usize| {
                let c = self.pixels[i];
                [c.r() as f32, c.g() as f32, c.b() as f32, c.a() as f32]
            };
            let (a, b, c, d) = (
                get(i),
                get(i + 1),
                get(i + w as usize),
                get(i + w as usize + 1),
            );
            if a[3] == 0.0 && b[3] == 0.0 && c[3] == 0.0 && d[3] == 0.0 {
                return [0.0; 4];
            }
            return std::array::from_fn(|k| {
                let top = a[k] + (b[k] - a[k]) * fx;
                let bottom = c[k] + (d[k] - c[k]) * fx;
                top + (bottom - top) * fy
            });
        }
        let (a, b, c, d) = (
            at(ix, iy),
            at(ix + 1, iy),
            at(ix, iy + 1),
            at(ix + 1, iy + 1),
        );
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            top + (bottom - top) * fy
        })
    }
}

/// `mesh` with its points at `pos`, drawn from `picture` (its triangles in
/// `order`, later ones over earlier ones where it folds): the tiles
/// (`tile_size` a side) of a `width`×`height` canvas it covers, with
/// anything at all on them. `step` above 1 is a quick rough draw (for a
/// big part while a pin is dragged): one pixel in each `step`×`step` block
/// worked out, and the block filled with it.
pub fn render(
    picture: &Picture,
    mesh: &PuppetMesh,
    pos: &[[f32; 2]],
    order: &[u32],
    tile_size: usize,
    (width, height): (usize, usize),
    step: usize,
) -> Vec<((i32, i32), Vec<Color32>)> {
    let step = step.max(1);
    let ts = tile_size as i32;
    let (tw, th) = ((width as i32 + ts - 1) / ts, (height as i32 + ts - 1) / ts);
    if tw <= 0 || th <= 0 {
        return Vec::new();
    }
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); (tw * th) as usize];
    for &i in order {
        let Some(t) = mesh.tris.get(i as usize) else {
            continue;
        };
        let p = t.map(|k| pos[k as usize]);
        let x0 = p.iter().map(|q| q[0]).fold(f32::MAX, f32::min);
        let x1 = p.iter().map(|q| q[0]).fold(f32::MIN, f32::max);
        let y0 = p.iter().map(|q| q[1]).fold(f32::MAX, f32::min);
        let y1 = p.iter().map(|q| q[1]).fold(f32::MIN, f32::max);
        if !(x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite())
            || x1 < 0.0
            || y1 < 0.0
            || x0 >= width as f32
            || y0 >= height as f32
        {
            continue;
        }
        let (ax, bx) = (
            (x0 as i32 / ts).clamp(0, tw - 1),
            (x1 as i32 / ts).clamp(0, tw - 1),
        );
        let (ay, by) = (
            (y0 as i32 / ts).clamp(0, th - 1),
            (y1 as i32 / ts).clamp(0, th - 1),
        );
        for ty in ay..=by {
            for tx in ax..=bx {
                buckets[(ty * tw + tx) as usize].push(i);
            }
        }
    }
    (0..buckets.len())
        .into_par_iter()
        .filter(|&b| !buckets[b].is_empty())
        .filter_map(|b| {
            let (tx, ty) = (b as i32 % tw, b as i32 / tw);
            let mut px = vec![[0.0f32; 4]; tile_size * tile_size];
            for &t in &buckets[b] {
                let tile = Tile {
                    origin: (tx * ts, ty * ts),
                    side: tile_size,
                    step,
                };
                fill(picture, mesh, pos, t as usize, tile, &mut px);
            }
            if step > 1 {
                // Each block (on the tile's own grid) from its top-left pixel.
                for ly in 0..tile_size {
                    let ay = ly - ly % step;
                    for lx in 0..tile_size {
                        px[ly * tile_size + lx] = px[ay * tile_size + lx - lx % step];
                    }
                }
            }
            let data: Vec<Color32> = px
                .iter()
                .map(|&[r, g, b, a]| {
                    // (`as` saturates: rounded and clamped to 0..=255.)
                    let q = |v: f32| (v + 0.5) as u8;
                    Color32::from_rgba_premultiplied(q(r), q(g), q(b), q(a))
                })
                .collect();
            data.iter().any(|p| p.a() > 0).then_some(((tx, ty), data))
        })
        .collect()
}

/// A tile being drawn: where it is on the canvas, its side, and the rough
/// draw's block size (1: every pixel).
#[derive(Clone, Copy)]
struct Tile {
    origin: (i32, i32),
    side: usize,
    step: usize,
}

/// Triangle `t` into a tile's pixels.
fn fill(
    picture: &Picture,
    mesh: &PuppetMesh,
    pos: &[[f32; 2]],
    t: usize,
    tile: Tile,
    px: &mut [[f32; 4]],
) {
    let Tile { origin, side, step } = tile;
    let idx = mesh.tris[t];
    let p = idx.map(|k| pos[k as usize]);
    let r = idx.map(|k| mesh.rest[k as usize]);
    // Edge `a → b` as `A·x + B·y + C`, worked out from its lower-numbered
    // point whichever triangle asks (so both triangles sharing it get the
    // very same numbers, and a pixel on it is inside exactly one), and the
    // sign that turns it the triangle's way.
    let edge = |a: usize, b: usize| -> ([f32; 3], f32) {
        let (lo, hi, sign) = if idx[a] < idx[b] {
            (p[a], p[b], 1.0)
        } else {
            (p[b], p[a], -1.0)
        };
        let (ea, eb) = (-(hi[1] - lo[1]), hi[0] - lo[0]);
        ([ea, eb, -(ea * lo[0] + eb * lo[1])], sign)
    };
    let eval = |e: &[f32; 3], x: f32, y: f32| e[0] * x + e[1] * y + e[2];
    // Edges opposite each point, and the triangle's (signed, doubled) area.
    let edges = [edge(1, 2), edge(2, 0), edge(0, 1)];
    let area = edges[2].1 * eval(&edges[2].0, p[2][0], p[2][1]);
    if area.abs() < 1e-9 {
        return;
    }
    // A pixel on an edge goes to the triangle on the edge's positive side
    // (seen from its lower-numbered point): of two sharing it, exactly one.
    let ties: [bool; 3] = std::array::from_fn(|k| eval(&edges[k].0, p[k][0], p[k][1]) > 0.0);
    // Inside means each oriented edge value has the area's sign.
    let flip: [f32; 3] = std::array::from_fn(|k| edges[k].1 * area.signum());
    // Where a canvas point came from at rest: an affine map, from the
    // weights (each edge's value over the area).
    let along = |c: usize| -> [f32; 3] {
        let mut m = [0.0f32; 3];
        for k in 0..3 {
            let s = edges[k].1 / area;
            for (mi, ei) in m.iter_mut().zip(edges[k].0) {
                *mi += r[k][c] * s * ei;
            }
        }
        m
    };
    let (mu, mv) = (along(0), along(1));
    let x0 = p.iter().map(|q| q[0]).fold(f32::MAX, f32::min);
    let x1 = p.iter().map(|q| q[0]).fold(f32::MIN, f32::max);
    let y0 = p.iter().map(|q| q[1]).fold(f32::MAX, f32::min);
    let y1 = p.iter().map(|q| q[1]).fold(f32::MIN, f32::max);
    let lx0 = (x0.floor() as i32 - origin.0).max(0);
    let lx1 = (x1.ceil() as i32 - origin.0 + 1).min(side as i32);
    let ly0 = (y0.floor() as i32 - origin.1).max(0);
    let ly1 = (y1.ceil() as i32 - origin.1 + 1).min(side as i32);
    // (A rough draw: only the pixels on the tile's block grid.)
    let on_grid = |l: i32| l + (step as i32 - l % step as i32) % step as i32;
    for ly in (on_grid(ly0)..ly1).step_by(step) {
        let y = (origin.1 + ly) as f32 + 0.5;
        // The row's span inside the triangle, a pixel wider each side (the
        // exact test below has the last word).
        let (mut sx0, mut sx1) = (lx0 as f32, lx1 as f32);
        for k in 0..3 {
            let [ea, eb, ec] = edges[k].0;
            let a = ea * flip[k];
            let rest = (eb * y + ec) * flip[k];
            // a·x + rest ≥ 0.
            if a.abs() < 1e-12 {
                if rest < 0.0 {
                    sx1 = sx0;
                }
                continue;
            }
            let at = -rest / a - origin.0 as f32 - 0.5;
            if a > 0.0 {
                sx0 = sx0.max(floor(at) - 1.0);
            } else {
                sx1 = sx1.min(floor(at) + 3.0);
            }
        }
        let (sx0, sx1) = ((sx0 as i32).max(lx0), (sx1 as i32).min(lx1));
        for lx in (on_grid(sx0)..sx1).step_by(step) {
            let x = (origin.0 + lx) as f32 + 0.5;
            let inside = (0..3).all(|k| {
                let v = eval(&edges[k].0, x, y) * flip[k];
                v > 0.0 || (v == 0.0 && ties[k])
            });
            if !inside {
                continue;
            }
            // Snap to the exact texel when the mesh is at rest there.
            let (u, v) = (snap(eval(&mu, x, y)), snap(eval(&mv, x, y)));
            let s = picture.sample(u, v);
            if s[3] <= 0.0 {
                continue;
            }
            let i = ly as usize * side + lx as usize;
            let keep = 1.0 - s[3] / 255.0;
            px[i] = std::array::from_fn(|k| s[k] + px[i][k] * keep);
        }
    }
}

/// A coordinate within float error of a pixel's middle, put on it.
fn snap(v: f32) -> f32 {
    // The nearest middle is the one in the pixel `v` is in.
    let m = floor(v) + 0.5;
    if (v - m).abs() < 1e-3 { m } else { v }
}

/// `v.floor()`, without a library call (pixel coordinates, well within
/// `i32`).
#[inline]
fn floor(v: f32) -> f32 {
    let t = v as i32 as f32;
    if t > v { t - 1.0 } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> (Picture, PuppetMesh) {
        // A soft-edged disc of colour on 96×64 pixels at (16, 8).
        let (x0, y0, w, h) = (16, 8, 96usize, 64usize);
        let pixels: Vec<Color32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let d = ((x - 48.0).powi(2) + (y - 32.0).powi(2)).sqrt();
                let a = ((28.0 - d) / 4.0).clamp(0.0, 1.0);
                Color32::from_rgba_unmultiplied(
                    (i % 251) as u8,
                    120,
                    (x * 2.0) as u8,
                    (a * 255.0) as u8,
                )
            })
            .collect();
        let pic = Picture {
            x0,
            y0,
            width: w,
            height: h,
            pixels,
        };
        let mesh = PuppetMesh::from_paint([x0, y0, x0 + w as i32, y0 + h as i32], 7.0, |x, y| {
            pic.pixels[((y - y0) as usize) * w + (x - x0) as usize].a() > 0
        })
        .unwrap();
        (pic, mesh)
    }

    fn all(mesh: &PuppetMesh) -> Vec<u32> {
        (0..mesh.tris.len() as u32).collect()
    }

    fn whole(
        tiles: &[((i32, i32), Vec<Color32>)],
        ts: usize,
        (w, h): (usize, usize),
    ) -> Vec<Color32> {
        let mut out = vec![Color32::TRANSPARENT; w * h];
        for ((tx, ty), data) in tiles {
            for (i, &c) in data.iter().enumerate() {
                let (x, y) = (*tx as usize * ts + i % ts, *ty as usize * ts + i / ts);
                if x < w && y < h {
                    out[y * w + x] = c;
                }
            }
        }
        out
    }

    #[test]
    fn at_rest_the_picture_comes_out_exactly() {
        let (pic, mesh) = picture();
        let size = (160, 96);
        let out = whole(
            &render(&pic, &mesh, &mesh.rest, &all(&mesh), 32, size, 1),
            32,
            size,
        );
        for y in 0..size.1 {
            for x in 0..size.0 {
                let want = if (16..112).contains(&x) && (8..72).contains(&y) {
                    pic.pixels[(y - 8) * 96 + (x - 16)]
                } else {
                    Color32::TRANSPARENT
                };
                assert_eq!(out[y * size.0 + x], want, "at {x},{y}");
            }
        }
    }

    #[test]
    fn a_rough_draw_fills_every_block_from_one_of_its_pixels() {
        let (pic, mesh) = picture();
        let size = (160, 96);
        let fine = whole(
            &render(&pic, &mesh, &mesh.rest, &all(&mesh), 32, size, 1),
            32,
            size,
        );
        let rough = whole(
            &render(&pic, &mesh, &mesh.rest, &all(&mesh), 32, size, 3),
            32,
            size,
        );
        for y in 0..size.1 {
            for x in 0..size.0 {
                // Its block's top-left, on its tile's own grid.
                let (ax, ay) = (x - (x % 32) % 3, y - (y % 32) % 3);
                assert_eq!(rough[y * size.0 + x], fine[ay * size.0 + ax], "at {x},{y}");
            }
        }
    }

    #[test]
    fn moved_whole_it_lands_moved() {
        let (pic, mesh) = picture();
        let size = (160, 96);
        let pos: Vec<[f32; 2]> = mesh
            .rest
            .iter()
            .map(|p| [p[0] + 10.0, p[1] + 5.0])
            .collect();
        let out = whole(
            &render(&pic, &mesh, &pos, &all(&mesh), 32, size, 1),
            32,
            size,
        );
        for y in 8..72usize {
            for x in 16..112usize {
                assert_eq!(
                    out[(y + 5) * size.0 + x + 10],
                    pic.pixels[(y - 8) * 96 + (x - 16)]
                );
            }
        }
    }
}
