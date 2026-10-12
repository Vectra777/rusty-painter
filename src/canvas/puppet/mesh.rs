//! A puppet's mesh, made from paint, as Photoshop's follows the drawing:
//! the paint grown a few pixels (so none of it is on the mesh's edge), its
//! outlines traced (holes too) and smoothed, points spread evenly inside
//! (a triangular lattice, so the triangles are near equilateral), and the
//! lot triangulated (constrained Delaunay) with the outlines as edges.
//! Parts that don't touch are separate pieces.
//!
//! Should the outline mesh ever miss a painted pixel (outlines crossing
//! where the paint nearly touches itself), a plain grid of square cells is
//! used instead: every painted pixel is always on the mesh.

use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

/// Most cells a grid mesh is made of: a bigger part gets bigger cells.
pub const MAX_CELLS: usize = 4000;
/// Most points an outline mesh has: a bigger part gets them further apart.
pub const MAX_POINTS: usize = 4000;
/// How far the mesh reaches past the paint, pixels.
const MARGIN: usize = 4;
/// How far a smoothed outline may stray from the traced one, pixels.
const SMOOTHING: f64 = 1.5;

/// Triangles over painted pixels, at rest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PuppetMesh {
    /// Each point, canvas pixels.
    pub rest: Vec<[f32; 2]>,
    pub tris: Vec<[u32; 3]>,
    /// Which piece each point belongs to (pieces don't share points).
    pub piece: Vec<u32>,
    pub pieces: usize,
    /// The cells' side, pixels.
    pub cell: f32,
}

impl PuppetMesh {
    /// The mesh over the pixels `painted(x, y)` says are painted, in
    /// `bounds` (`[x0, y0, x1, y1)`, canvas pixels), its points about
    /// `cell` pixels apart (further when the area would need more than
    /// [`MAX_POINTS`]). `None` when nothing is painted.
    pub fn from_paint(
        bounds: [i32; 4],
        cell: f32,
        painted: impl Fn(i32, i32) -> bool,
    ) -> Option<PuppetMesh> {
        let [x0, y0, x1, y1] = bounds;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        if w == 0 || h == 0 {
            return None;
        }
        PuppetShape::from_paint([x0, y0, x0 + w as i32, y0 + h as i32], cell, 0, painted)?
            .mesh(&[])
            .map(|(m, _)| m)
    }

    /// A mesh of these points and triangles as they are (a rig part's),
    /// its pieces worked out.
    pub fn from_triangles(rest: Vec<[f32; 2]>, tris: Vec<[u32; 3]>) -> PuppetMesh {
        let tris: Vec<[u32; 3]> = tris
            .into_iter()
            .filter(|t| t.iter().all(|&k| (k as usize) < rest.len()))
            .collect();
        let (piece, pieces) = pieces(rest.len(), &tris);
        let cell = tris
            .first()
            .map_or(1.0, |t| {
                dist2(rest[t[0] as usize], rest[t[1] as usize]).sqrt()
            })
            .max(1.0);
        PuppetMesh {
            rest,
            tris,
            piece,
            pieces,
            cell,
        }
    }

    /// Whether every painted pixel's middle is on the mesh.
    fn covers(&self, [x0, y0, x1, y1]: [i32; 4], paint: &[bool]) -> bool {
        let w = (x1 - x0) as usize;
        let mut on = vec![false; paint.len()];
        for t in &self.tris {
            let [a, b, c] = t.map(|i| self.rest[i as usize]);
            let lo_x = (a[0].min(b[0]).min(c[0]).floor() as i32).max(x0);
            let hi_x = (a[0].max(b[0]).max(c[0]).ceil() as i32).min(x1);
            let lo_y = (a[1].min(b[1]).min(c[1]).floor() as i32).max(y0);
            let hi_y = (a[1].max(b[1]).max(c[1]).ceil() as i32).min(y1);
            for y in lo_y..hi_y {
                for x in lo_x..hi_x {
                    if inside([x as f32 + 0.5, y as f32 + 0.5], a, b, c) {
                        on[(y - y0) as usize * w + (x - x0) as usize] = true;
                    }
                }
            }
        }
        paint.iter().zip(&on).all(|(&p, &o)| !p || o)
    }

    /// A grid of square cells, `cell` pixels a side at least: every cell
    /// with paint in it and a ring round them, each cut into two triangles.
    fn grid(bounds: [i32; 4], cell: f32, painted: impl Fn(i32, i32) -> bool) -> Option<PuppetMesh> {
        let [x0, y0, x1, y1] = bounds;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        if w == 0 || h == 0 {
            return None;
        }
        let cell = cell
            .max(2.0)
            .max(((w * h) as f32 / MAX_CELLS as f32).sqrt());
        let side = cell.ceil() as usize;
        // One cell of margin all round, for the ring.
        let (gw, gh) = (w.div_ceil(side) + 2, h.div_ceil(side) + 2);
        let (ox, oy) = (x0 - side as i32, y0 - side as i32);
        let mut paint = vec![false; gw * gh];
        for y in 0..h {
            let cy = y / side + 1;
            for x in 0..w {
                if painted(x0 + x as i32, y0 + y as i32) {
                    paint[cy * gw + x / side + 1] = true;
                }
            }
        }
        if !paint.iter().any(|&p| p) {
            return None;
        }
        // The ring: cells next to paint (corners included).
        let mut keep = paint.clone();
        for cy in 0..gh {
            for cx in 0..gw {
                if !paint[cy * gw + cx] {
                    continue;
                }
                for ny in cy.saturating_sub(1)..=(cy + 1).min(gh - 1) {
                    for nx in cx.saturating_sub(1)..=(cx + 1).min(gw - 1) {
                        keep[ny * gw + nx] = true;
                    }
                }
            }
        }
        // Grid corners → points, made as cells need them.
        let mut index = vec![u32::MAX; (gw + 1) * (gh + 1)];
        let mut rest = Vec::new();
        let mut point = |i: usize, j: usize, rest: &mut Vec<[f32; 2]>| -> u32 {
            let k = j * (gw + 1) + i;
            if index[k] == u32::MAX {
                index[k] = rest.len() as u32;
                rest.push([
                    (ox + (i * side) as i32) as f32,
                    (oy + (j * side) as i32) as f32,
                ]);
            }
            index[k]
        };
        let mut tris = Vec::new();
        for cy in 0..gh {
            for cx in 0..gw {
                if !keep[cy * gw + cx] {
                    continue;
                }
                let a = point(cx, cy, &mut rest);
                let b = point(cx + 1, cy, &mut rest);
                let c = point(cx + 1, cy + 1, &mut rest);
                let d = point(cx, cy + 1, &mut rest);
                // Diagonals alternate, so the mesh bends the same either way.
                if (cx + cy) % 2 == 0 {
                    tris.push([a, b, c]);
                    tris.push([a, c, d]);
                } else {
                    tris.push([a, b, d]);
                    tris.push([b, c, d]);
                }
            }
        }
        let (piece, pieces) = pieces(rest.len(), &tris);
        Some(PuppetMesh {
            rest,
            tris,
            piece,
            pieces,
            cell: side as f32,
        })
    }

    /// Each point's neighbours (along triangle edges), sorted.
    pub fn neighbours(&self) -> Vec<Vec<u32>> {
        let mut out: Vec<Vec<u32>> = vec![Vec::new(); self.rest.len()];
        for t in &self.tris {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                out[a as usize].push(b);
                out[b as usize].push(a);
            }
        }
        for n in &mut out {
            n.sort_unstable();
            n.dedup();
        }
        out
    }

    /// The point nearest `p` among `positions` (the mesh as it is now), if
    /// `p` is on the mesh (inside one of its triangles there).
    pub fn point_at(&self, positions: &[[f32; 2]], p: [f32; 2]) -> Option<usize> {
        let on = self.tris.iter().any(|t| {
            let [a, b, c] = t.map(|i| positions[i as usize]);
            inside(p, a, b, c)
        });
        if !on {
            return None;
        }
        (0..positions.len())
            .min_by(|&i, &j| dist2(positions[i], p).total_cmp(&dist2(positions[j], p)))
    }
}

/// A part's outlines and the points spread inside them, worked out once:
/// meshes are made from it with extra points (pins) where they're wanted.
#[derive(Clone, Debug)]
pub struct Outline {
    /// The grid's top-left on the canvas (outline points are on the grid).
    origin: [f64; 2],
    rings: Vec<Vec<[f64; 2]>>,
    /// Points inside, kept clear of the outlines.
    lattice: Vec<[f64; 2]>,
    spacing: f64,
}

impl Outline {
    /// The outlines of `paint` (`bounds`' pixels, row by row) grown by
    /// `margin`, with points about `cell` apart.
    fn new(bounds: [i32; 4], paint: &[bool], cell: f32, margin: usize) -> Option<Outline> {
        let [x0, y0, x1, y1] = bounds;
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        // The paint grown by the margin, on a grid with room for it (and
        // one empty pixel more all round, so outlines close).
        let pad = margin + 1;
        let (gw, gh) = (w + 2 * pad, h + 2 * pad);
        let grown = grow(paint, w, h, pad, margin);
        let area = grown.iter().filter(|&&g| g).count();
        let spacing =
            (cell.max(4.0) as f64).max((area as f64 / (MAX_POINTS as f64 * 0.866)).sqrt());
        let rings: Vec<Vec<[f64; 2]>> = trace(&grown, gw, gh)
            .into_iter()
            .map(|ring| simplify(&ring, SMOOTHING))
            .filter(|ring| ring.len() >= 3 && ring_area(ring).abs() >= 4.0)
            .map(|ring| subdivide(&ring, spacing))
            .collect();
        if rings.is_empty() {
            return None;
        }
        // Points inside, on a triangular lattice, kept clear of the outline.
        let clear = spacing * 0.6;
        let is_in = |x: f64, y: f64| {
            let (gx, gy) = (x.floor() as i64, y.floor() as i64);
            gx >= 0
                && gy >= 0
                && (gx as usize) < gw
                && (gy as usize) < gh
                && grown[gy as usize * gw + gx as usize]
        };
        let mut lattice = Vec::new();
        let row = spacing * 0.866;
        let mut j = 0;
        let mut y = row / 2.0;
        while y < gh as f64 {
            let shift = if j % 2 == 1 { spacing / 2.0 } else { 0.0 };
            let mut x = spacing / 2.0 + shift;
            while x < gw as f64 {
                let far = (0..12).all(|k| {
                    let a = k as f64 * std::f64::consts::TAU / 12.0;
                    is_in(x + clear * a.cos(), y + clear * a.sin())
                });
                if is_in(x, y) && far {
                    lattice.push([x, y]);
                }
                x += spacing;
            }
            y += row;
            j += 1;
        }
        Some(Outline {
            origin: [(x0 - pad as i32) as f64, (y0 - pad as i32) as f64],
            rings,
            lattice,
            spacing,
        })
    }

    /// Whether canvas point `p` is inside the outlines.
    fn contains(&self, p: [f64; 2]) -> bool {
        let q = [p[0] - self.origin[0], p[1] - self.origin[1]];
        self.rings.iter().fold(false, |acc, r| acc ^ in_ring(q, r))
    }

    /// The mesh, with points at `extra` too (canvas points; those outside
    /// the outlines are left out): and each extra point's place in it.
    fn mesh(&self, extra: &[[f32; 2]]) -> Option<(PuppetMesh, Vec<Option<usize>>)> {
        let [ox, oy] = self.origin;
        let spacing = self.spacing;
        let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> =
            ConstrainedDelaunayTriangulation::new();
        for ring in &self.rings {
            let handles: Vec<_> = ring
                .iter()
                .map(|p| cdt.insert(Point2::new(p[0], p[1])))
                .collect::<Result<_, _>>()
                .ok()?;
            for k in 0..handles.len() {
                let (a, b) = (handles[k], handles[(k + 1) % handles.len()]);
                if a != b {
                    // An edge that would cross another is left out (the
                    // coverage check catches what that loses).
                    cdt.try_add_constraint(a, b);
                }
            }
        }
        let extra: Vec<[f64; 2]> = extra
            .iter()
            .map(|p| [p[0] as f64 - ox, p[1] as f64 - oy])
            .collect();
        // Lattice points near an extra one make way for it.
        let room = (spacing * 0.4).powi(2);
        for q in &self.lattice {
            let crowded = extra
                .iter()
                .any(|e| (e[0] - q[0]).powi(2) + (e[1] - q[1]).powi(2) < room);
            if !crowded {
                cdt.insert(Point2::new(q[0], q[1])).ok()?;
            }
        }
        let extra_handles: Vec<Option<_>> = extra
            .iter()
            .map(|e| {
                let inside = self.rings.iter().fold(false, |acc, r| acc ^ in_ring(*e, r));
                if inside {
                    cdt.insert(Point2::new(e[0], e[1])).ok()
                } else {
                    None
                }
            })
            .collect();
        // The triangles inside the outlines (holes are outlines too: inside
        // means inside an odd number of them).
        let mut index = vec![u32::MAX; cdt.num_vertices()];
        let mut rest = Vec::new();
        let mut tris = Vec::new();
        for face in cdt.inner_faces() {
            let v = face.vertices();
            let p = v.map(|v| v.position());
            let area =
                (p[1].x - p[0].x) * (p[2].y - p[0].y) - (p[1].y - p[0].y) * (p[2].x - p[0].x);
            // (Three points along one straight stretch of outline: nothing.)
            if area.abs() < 1e-3 * spacing * spacing {
                continue;
            }
            let mid = [
                (p[0].x + p[1].x + p[2].x) / 3.0,
                (p[0].y + p[1].y + p[2].y) / 3.0,
            ];
            if !self
                .rings
                .iter()
                .fold(false, |acc, r| acc ^ in_ring(mid, r))
            {
                continue;
            }
            let t = v.map(|v| {
                let i = v.fix().index();
                if index[i] == u32::MAX {
                    index[i] = rest.len() as u32;
                    let q = v.position();
                    rest.push([(q.x + ox) as f32, (q.y + oy) as f32]);
                }
                index[i]
            });
            tris.push(t);
        }
        if tris.is_empty() {
            return None;
        }
        let (piece, pieces) = pieces(rest.len(), &tris);
        let mesh = PuppetMesh {
            rest,
            tris,
            piece,
            pieces,
            cell: spacing as f32,
        };
        let places = extra_handles
            .iter()
            .map(|h| {
                h.map(|h| index[h.index()])
                    .filter(|&i| i != u32::MAX)
                    .map(|i| i as usize)
            })
            .collect();
        Some((mesh, places))
    }
}

/// What a part's meshes are made from: its outline, or (where the outline
/// mesh would miss paint) a fixed grid, pins going to its nearest points.
#[derive(Clone, Debug)]
pub enum PuppetShape {
    Outline(Outline),
    Grid(PuppetMesh),
}

impl PuppetShape {
    /// The shape of the pixels `painted(x, y)` says are painted, in
    /// `bounds` (`[x0, y0, x1, y1)`), grown by `expansion` pixels past the
    /// usual margin, its points about `cell` pixels apart. `None` when
    /// nothing is painted.
    pub fn from_paint(
        bounds: [i32; 4],
        cell: f32,
        expansion: usize,
        painted: impl Fn(i32, i32) -> bool,
    ) -> Option<PuppetShape> {
        let [x0, y0, x1, y1] = bounds;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        if w == 0 || h == 0 {
            return None;
        }
        let paint: Vec<bool> = (0..w * h)
            .map(|i| painted(x0 + (i % w) as i32, y0 + (i / w) as i32))
            .collect();
        if !paint.iter().any(|&p| p) {
            return None;
        }
        let margin = MARGIN + expansion;
        if let Some(outline) = Outline::new(bounds, &paint, cell, margin)
            && outline
                .mesh(&[])
                .is_some_and(|(m, _)| m.covers(bounds, &paint))
        {
            return Some(PuppetShape::Outline(outline));
        }
        let at = |x: i32, y: i32| {
            x >= x0
                && y >= y0
                && x < x1
                && y < y1
                && paint[(y - y0) as usize * w + (x - x0) as usize]
        };
        PuppetMesh::grid(bounds, cell.max((margin as f32 / 1.5).max(1.0)), at)
            .map(PuppetShape::Grid)
    }

    /// The mesh with a point at each of `extra` where it can (the grid: the
    /// nearest of its own), and each one's point.
    pub fn mesh(&self, extra: &[[f32; 2]]) -> Option<(PuppetMesh, Vec<Option<usize>>)> {
        match self {
            PuppetShape::Outline(o) => o.mesh(extra),
            PuppetShape::Grid(m) => {
                let places = extra.iter().map(|&p| m.point_at(&m.rest, p)).collect();
                Some((m.clone(), places))
            }
        }
    }

    /// Whether canvas point `p` is on the part (at rest).
    pub fn contains(&self, p: [f32; 2]) -> bool {
        match self {
            PuppetShape::Outline(o) => o.contains([p[0] as f64, p[1] as f64]),
            PuppetShape::Grid(m) => m.point_at(&m.rest, p).is_some(),
        }
    }
}

/// `paint` (`w`×`h`) grown by `by` pixels (a square reach), on a grid
/// `pad` pixels bigger each side.
fn grow(paint: &[bool], w: usize, h: usize, pad: usize, by: usize) -> Vec<bool> {
    let (gw, gh) = (w + 2 * pad, h + 2 * pad);
    // Along rows, then down columns: how many painted pixels are within
    // reach, by running counts.
    let mut across = vec![false; gw * gh];
    for y in 0..h {
        let src = &paint[y * w..(y + 1) * w];
        let dst = &mut across[(y + pad) * gw..(y + pad + 1) * gw];
        let mut last: Option<usize> = None;
        // Nearest painted pixel to the left (or here), then to the right.
        for (gx, d) in dst.iter_mut().enumerate() {
            let x = gx as isize - pad as isize;
            if x >= 0 && (x as usize) < w && src[x as usize] {
                last = Some(gx);
            }
            *d = last.is_some_and(|l| gx - l <= by);
        }
        last = None;
        for gx in (0..gw).rev() {
            let x = gx as isize - pad as isize;
            if x >= 0 && (x as usize) < w && src[x as usize] {
                last = Some(gx);
            }
            if last.is_some_and(|l| l - gx <= by) {
                dst[gx] = true;
            }
        }
    }
    let mut out = vec![false; gw * gh];
    for x in 0..gw {
        let mut last: Option<usize> = None;
        for y in 0..gh {
            if across[y * gw + x] {
                last = Some(y);
            }
            out[y * gw + x] = last.is_some_and(|l| y - l <= by);
        }
        last = None;
        for y in (0..gh).rev() {
            if across[y * gw + x] {
                last = Some(y);
            }
            if last.is_some_and(|l| l - y <= by) {
                out[y * gw + x] = true;
            }
        }
    }
    out
}

/// The outlines of `mask` (`w`×`h`, its edge pixels empty) along pixel
/// edges, as closed rings of pixel corners: outsides one way round, holes
/// the other.
fn trace(mask: &[bool], w: usize, h: usize) -> Vec<Vec<[f64; 2]>> {
    let at = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && mask[y as usize * w + x as usize]
    };
    // Edges with the paint on their right (y down): from corner, to corner.
    let corner = |x: usize, y: usize| (y * (w + 1) + x) as u32;
    let mut from = Vec::new();
    let mut to = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            let (xi, yi) = (x as isize, y as isize);
            if !at(xi, yi - 1) {
                from.push(corner(x, y));
                to.push(corner(x + 1, y));
            }
            if !at(xi + 1, yi) {
                from.push(corner(x + 1, y));
                to.push(corner(x + 1, y + 1));
            }
            if !at(xi, yi + 1) {
                from.push(corner(x + 1, y + 1));
                to.push(corner(x, y + 1));
            }
            if !at(xi - 1, yi) {
                from.push(corner(x, y + 1));
                to.push(corner(x, y));
            }
        }
    }
    // Edges leaving each corner (two where pixels touch only diagonally).
    let mut out: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (e, &f) in from.iter().enumerate() {
        out.entry(f).or_default().push(e);
    }
    let pos = |c: u32| [(c as usize % (w + 1)) as f64, (c as usize / (w + 1)) as f64];
    let mut used = vec![false; from.len()];
    let mut rings = Vec::new();
    for start in 0..from.len() {
        if used[start] {
            continue;
        }
        let mut ring = Vec::new();
        let mut e = start;
        loop {
            used[e] = true;
            ring.push(pos(from[e]));
            let here = to[e];
            // Where two edges leave, take the one turning right, keeping
            // diagonal neighbours apart.
            let dir = |e: usize| {
                let (a, b) = (pos(from[e]), pos(to[e]));
                [b[0] - a[0], b[1] - a[1]]
            };
            let d = dir(e);
            let next = out.get(&here).and_then(|list| {
                let free: Vec<usize> = list.iter().copied().filter(|&n| !used[n]).collect();
                free.iter()
                    .copied()
                    .find(|&n| {
                        let nd = dir(n);
                        d[0] * nd[1] - d[1] * nd[0] > 0.0
                    })
                    .or_else(|| free.first().copied())
            });
            match next {
                Some(n) => e = n,
                None => break,
            }
        }
        // Only the corners where the outline turns.
        let n = ring.len();
        let turns: Vec<[f64; 2]> = (0..n)
            .filter(|&i| {
                let (a, b, c) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
                (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]) != 0.0
            })
            .map(|i| ring[i])
            .collect();
        if turns.len() >= 3 {
            rings.push(turns);
        }
    }
    rings
}

/// A closed ring with points dropped while it stays within `tolerance`
/// of the original (Douglas–Peucker, from its two furthest-apart points).
fn simplify(ring: &[[f64; 2]], tolerance: f64) -> Vec<[f64; 2]> {
    let n = ring.len();
    if n < 4 {
        return ring.to_vec();
    }
    let d2 = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2);
    let far = (1..n)
        .max_by(|&i, &j| d2(ring[0], ring[i]).total_cmp(&d2(ring[0], ring[j])))
        .unwrap_or(n / 2);
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[far] = true;
    // Each half: indices along the ring from `a` to `b`.
    let mut stack = vec![(0, far), (far, n)];
    while let Some((a, b)) = stack.pop() {
        if b - a < 2 {
            continue;
        }
        let (p, q) = (ring[a], ring[b % n]);
        let len = d2(p, q).sqrt();
        let (mut worst, mut at) = (0.0, a);
        for (i, &r) in ring.iter().enumerate().take(b).skip(a + 1) {
            let dist = if len < 1e-9 {
                d2(p, r).sqrt()
            } else {
                ((q[0] - p[0]) * (p[1] - r[1]) - (p[0] - r[0]) * (q[1] - p[1])).abs() / len
            };
            if dist > worst {
                (worst, at) = (dist, i);
            }
        }
        if worst > tolerance {
            keep[at] = true;
            stack.push((a, at));
            stack.push((at, b));
        }
    }
    (0..n).filter(|&i| keep[i]).map(|i| ring[i]).collect()
}

/// Twice the ring's signed area.
fn ring_area(ring: &[[f64; 2]]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

/// The ring with points added so no side is longer than `spacing`.
fn subdivide(ring: &[[f64; 2]], spacing: f64) -> Vec<[f64; 2]> {
    let n = ring.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let parts = (len / spacing).ceil().max(1.0) as usize;
        for k in 0..parts {
            let t = k as f64 / parts as f64;
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    out
}

/// Whether `p` is inside the ring (even–odd).
fn in_ring(p: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let n = ring.len();
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn dist2(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Whether `p` is in triangle `abc` (either way round, edges included).
fn inside(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let cross = |o: [f32; 2], u: [f32; 2], v: [f32; 2]| {
        (u[0] - o[0]) * (v[1] - o[1]) - (u[1] - o[1]) * (v[0] - o[0])
    };
    let (d0, d1, d2) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
    let neg = d0 < 0.0 || d1 < 0.0 || d2 < 0.0;
    let pos = d0 > 0.0 || d1 > 0.0 || d2 > 0.0;
    !(neg && pos)
}

/// Which connected piece each point is in, and how many there are.
fn pieces(n: usize, tris: &[[u32; 3]]) -> (Vec<u32>, usize) {
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for t in tris {
        for k in 1..3 {
            let (a, b) = (
                root(&mut parent, t[0] as usize),
                root(&mut parent, t[k] as usize),
            );
            if a != b {
                parent[a] = b;
            }
        }
    }
    let mut label = vec![u32::MAX; n];
    let mut count = 0;
    let mut piece = vec![0; n];
    for (i, p) in piece.iter_mut().enumerate() {
        let r = root(&mut parent, i);
        if label[r] == u32::MAX {
            label[r] = count;
            count += 1;
        }
        *p = label[r];
    }
    (piece, count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(cx: f32, cy: f32, r: f32) -> impl Fn(i32, i32) -> bool {
        move |x, y| (x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2) < r * r
    }

    /// Whether pixel `(x, y)`'s middle is on the mesh.
    fn covered(m: &PuppetMesh, x: i32, y: i32) -> bool {
        let p = [x as f32 + 0.5, y as f32 + 0.5];
        m.tris.iter().any(|t| {
            let [a, b, c] = t.map(|i| m.rest[i as usize]);
            inside(p, a, b, c)
        })
    }

    #[test]
    fn the_mesh_covers_the_paint_with_a_margin_and_little_else() {
        let paint = disc(100.0, 80.0, 40.0);
        let m = PuppetMesh::from_paint([0, 0, 200, 160], 10.0, &paint).unwrap();
        for y in 0..160 {
            for x in 0..200 {
                if paint(x, y) {
                    assert!(covered(&m, x, y), "paint at {x},{y} left out");
                }
            }
        }
        // Nothing far from the disc (more than two cells out).
        for (x, y) in [(5, 5), (190, 150), (100, 10), (30, 80)] {
            assert!(!covered(&m, x, y), "{x},{y} is far from the paint");
        }
        // And no paint on its edge: every pixel next to paint is covered too.
        for y in 1..159 {
            for x in 1..199 {
                if paint(x, y) {
                    assert!(covered(&m, x - 1, y - 1) && covered(&m, x + 1, y + 1));
                }
            }
        }
        assert_eq!(m.pieces, 1);
    }

    #[test]
    fn parts_apart_are_separate_pieces() {
        let (a, b) = (disc(40.0, 40.0, 20.0), disc(160.0, 40.0, 20.0));
        let m = PuppetMesh::from_paint([0, 0, 200, 80], 8.0, |x, y| a(x, y) || b(x, y)).unwrap();
        assert_eq!(m.pieces, 2);
        let left = m.point_at(&m.rest, [40.0, 40.0]).unwrap();
        let right = m.point_at(&m.rest, [160.0, 40.0]).unwrap();
        assert_ne!(m.piece[left], m.piece[right]);
        assert!(m.point_at(&m.rest, [100.0, 40.0]).is_none(), "between them");
    }

    #[test]
    fn a_big_part_gets_its_points_further_apart() {
        let m = PuppetMesh::from_paint([0, 0, 4000, 4000], 10.0, |_, _| true).unwrap();
        assert!(
            m.rest.len() <= MAX_POINTS * 12 / 10,
            "{} points",
            m.rest.len()
        );
        assert!(m.cell > 10.0);
    }

    #[test]
    fn the_mesh_follows_the_outline_with_even_triangles() {
        let paint = disc(100.0, 80.0, 50.0);
        let m = PuppetMesh::from_paint([0, 0, 200, 160], 12.0, &paint).unwrap();
        // The outline, not a staircase of cells: no point further out than
        // the margin and the smoothing.
        for p in &m.rest {
            let d = ((p[0] - 100.0).powi(2) + (p[1] - 80.0).powi(2)).sqrt();
            assert!(
                d <= 50.0 + MARGIN as f32 * 1.5 + SMOOTHING as f32 + 1.0,
                "{p:?} at {d}"
            );
        }
        // No slivers: every angle at least 15°.
        for t in &m.tris {
            let p = t.map(|i| m.rest[i as usize]);
            for k in 0..3 {
                let (a, b, c) = (p[k], p[(k + 1) % 3], p[(k + 2) % 3]);
                let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - a[0], c[1] - a[1]]);
                let cos = (u[0] * v[0] + u[1] * v[1]) / ((u[0].hypot(u[1])) * (v[0].hypot(v[1])));
                assert!(cos.acos().to_degrees() >= 15.0, "{p:?}");
            }
        }
        // About as many points as the spacing asks for.
        let expected = std::f32::consts::PI * 54.0 * 54.0 / (12.0 * 12.0 * 0.866);
        assert!(
            (m.rest.len() as f32) < expected * 1.6,
            "{} points",
            m.rest.len()
        );
    }

    #[test]
    fn a_hole_in_the_paint_is_a_hole_in_the_mesh() {
        let ring = |x: i32, y: i32| {
            let d = ((x as f32 + 0.5 - 100.0).powi(2) + (y as f32 + 0.5 - 100.0).powi(2)).sqrt();
            (30.0..80.0).contains(&d)
        };
        let m = PuppetMesh::from_paint([0, 0, 200, 200], 10.0, ring).unwrap();
        assert!(m.point_at(&m.rest, [100.0, 100.0]).is_none(), "the hole");
        assert!(m.point_at(&m.rest, [100.0, 45.0]).is_some(), "the ring");
        assert_eq!(m.pieces, 1);
    }

    #[test]
    fn nothing_painted_is_no_mesh() {
        assert!(PuppetMesh::from_paint([0, 0, 50, 50], 10.0, |_, _| false).is_none());
    }
}
