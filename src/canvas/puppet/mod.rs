//! Puppet warp: a mesh over a painted part, bent by pins placed on it,
//! the rest following as rigidly as it can ([`arap`]). A pin is a point of
//! the mesh exactly where it was placed (the mesh is made again round it).
//! Pins can be turned, which turns the paint round them, and given a depth:
//! where the bent paint folds over itself, the part nearer a deeper pin
//! goes on top. Pieces of the part with no pin stay where they are.

pub mod arap;
pub mod mesh;
pub mod raster;

use arap::{PuppetMode, Solver};
use mesh::{PuppetMesh, PuppetShape};

/// A pin: the part's point `rest` (canvas pixels, unbent) held at `at`,
/// turned `turn` radians if it's fixed (holding its neighbours round it
/// too; `None` lets the bend turn it), and its depth.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pin {
    pub rest: [f32; 2],
    pub at: [f32; 2],
    pub turn: Option<f32>,
    #[serde(default)]
    pub depth: i32,
    /// Its point in the current mesh.
    #[serde(skip)]
    pub point: usize,
}

/// Tiles of a layer's pixels (premultiplied), by tile.
pub type Tiles = std::collections::HashMap<(i32, i32), Vec<eframe::egui::Color32>>;

/// A selection's coverage (0..=255) over a box, kept with a warp.
#[derive(Clone, Debug, PartialEq)]
pub struct Coverage {
    pub x0: i32,
    pub y0: i32,
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Coverage {
    /// Coverage of canvas pixel `(x, y)` (none outside the box).
    pub fn value(&self, x: i32, y: i32) -> u8 {
        let (lx, ly) = (x - self.x0, y - self.y0);
        if lx < 0 || ly < 0 || lx as usize >= self.w || ly as usize >= self.h {
            return 0;
        }
        self.data[ly as usize * self.w + lx as usize]
    }

    /// The box, `[x0, y0, x1, y1)`.
    pub fn bounds(&self) -> [i32; 4] {
        [
            self.x0,
            self.y0,
            self.x0 + self.w as i32,
            self.y0 + self.h as i32,
        ]
    }
}

/// A warp a layer remembers, so Puppet Warp can open it again: the paint
/// it bent (shared by the warps made from it), the part of it bent (a
/// selection's coverage, or all of it), the pins and settings, and a
/// checksum of the layer it made. It's the one reopened while the layer's
/// pixels still match that checksum (so undo, which brings a layer's
/// pixels back, brings its warp back too; and any other edit leaves it
/// behind).
#[derive(Clone, Debug)]
pub struct WarpMemory {
    pub original: std::sync::Arc<Tiles>,
    pub part: Option<std::sync::Arc<Coverage>>,
    pub pins: Vec<Pin>,
    pub mode: PuppetMode,
    pub density: f32,
    pub expansion: f32,
    pub result: u32,
}

/// Most warps a layer remembers (the latest).
pub const REMEMBERED: usize = 8;

/// A checksum of a layer's pixels (its tiles with any paint on them).
pub fn checksum(tiles: &Tiles) -> u32 {
    let mut keys: Vec<&(i32, i32)> = tiles
        .iter()
        .filter(|(_, t)| t.iter().any(|c| c.a() > 0))
        .map(|(k, _)| k)
        .collect();
    keys.sort_unstable();
    let mut h = crc32fast::Hasher::new();
    for k in keys {
        h.update(&k.0.to_le_bytes());
        h.update(&k.1.to_le_bytes());
        for c in &tiles[k] {
            h.update(&c.to_array());
        }
    }
    h.finalize()
}

/// A part's shape, its mesh, its pins and where they've bent it.
pub struct Puppet {
    shape: PuppetShape,
    pub mesh: PuppetMesh,
    nbrs: Vec<Vec<u32>>,
    /// Where each mesh point is now.
    pub pos: Vec<[f32; 2]>,
    pins: Vec<Pin>,
    /// The order the triangles are drawn in (deeper pins' parts last).
    order: Vec<u32>,
    /// The solver for the current set of held points.
    solver: Option<Solver>,
    held: Vec<bool>,
    /// A fixed mesh (a rig part's): never made again, pins going to its
    /// points; pieces with no pin stay where they started.
    fixed: Option<Vec<[f32; 2]>>,
}

impl Puppet {
    pub fn new(shape: PuppetShape) -> Option<Self> {
        let (mesh, _) = shape.mesh(&[])?;
        let mut p = Self {
            shape,
            nbrs: mesh.neighbours(),
            pos: mesh.rest.clone(),
            held: vec![false; mesh.rest.len()],
            order: (0..mesh.tris.len() as u32).collect(),
            mesh,
            pins: Vec::new(),
            solver: None,
            fixed: None,
        };
        p.reorder();
        Some(p)
    }

    /// A puppet of a mesh that's never made again (a rig part's), its
    /// points now at `pos` (bent already, perhaps).
    pub fn fixed(mesh: PuppetMesh, pos: Vec<[f32; 2]>) -> Option<Self> {
        if mesh.tris.is_empty() || pos.len() != mesh.rest.len() {
            return None;
        }
        let mut p = Self {
            shape: PuppetShape::Grid(mesh.clone()),
            nbrs: mesh.neighbours(),
            held: vec![false; mesh.rest.len()],
            order: (0..mesh.tris.len() as u32).collect(),
            fixed: Some(pos.clone()),
            pos,
            mesh,
            pins: Vec::new(),
            solver: None,
        };
        p.reorder();
        Some(p)
    }

    /// Pins on a fixed mesh's points (each held where the mesh has it now).
    pub fn pin_points(&mut self, points: &[usize]) {
        self.pins = (points.iter())
            .filter(|&&k| k < self.pos.len())
            .map(|&k| Pin {
                rest: self.mesh.rest[k],
                at: self.pos[k],
                turn: None,
                depth: 0,
                point: k,
            })
            .collect();
        self.pins.dedup_by_key(|p| p.point);
        self.solver = None;
        self.reorder();
    }

    pub fn pins(&self) -> &[Pin] {
        &self.pins
    }

    /// The triangles in the order they're drawn.
    pub fn order(&self) -> &[u32] {
        &self.order
    }

    /// The pin within `radius` of `p` (the nearest), if any.
    pub fn pin_near(&self, p: [f32; 2], radius: f32) -> Option<usize> {
        let d = |q: [f32; 2]| ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
        (0..self.pins.len())
            .map(|i| (i, d(self.pins[i].at)))
            .filter(|&(_, dist)| dist <= radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// A pin at `p` (where the bent part is now), if `p` is on it: the pin
    /// already there if there is one.
    pub fn add_pin(&mut self, p: [f32; 2]) -> Option<usize> {
        if self.fixed.is_some() {
            // The nearest point of the mesh, held where it is.
            let point = self.mesh.point_at(&self.pos, p)?;
            if let Some(i) = self.pins.iter().position(|q| q.point == point) {
                return Some(i);
            }
            self.pins.push(Pin {
                rest: self.mesh.rest[point],
                at: self.pos[point],
                turn: None,
                depth: 0,
                point,
            });
            self.solver = None;
            self.reorder();
            return Some(self.pins.len() - 1);
        }
        // Where `p` is on the part at rest.
        let (t, w) = locate(&self.pos, &self.mesh.tris, &[p])[0]?;
        let r = self.mesh.tris[t].map(|k| self.mesh.rest[k as usize]);
        let rest = [
            r[0][0] * w[0] + r[1][0] * w[1] + r[2][0] * w[2],
            r[0][1] * w[0] + r[1][1] * w[1] + r[2][1] * w[2],
        ];
        if let Some(i) = self
            .pins
            .iter()
            .position(|q| (q.rest[0] - rest[0]).abs() < 0.5 && (q.rest[1] - rest[1]).abs() < 0.5)
        {
            return Some(i);
        }
        self.pins.push(Pin {
            rest,
            at: p,
            turn: None,
            depth: 0,
            point: 0,
        });
        self.rebuild();
        // (On a grid shape, two pins can land on one point: the new one
        // gives way to the pin already there.)
        self.pins.iter().position(|q| q.rest == rest).or_else(|| {
            let point = nearest(&self.mesh.rest, rest);
            self.pins.iter().position(|q| q.point == point)
        })
    }

    /// Take pins out (by index).
    pub fn remove_pins(&mut self, which: &[usize]) {
        let mut i = 0;
        self.pins.retain(|_| {
            i += 1;
            !which.contains(&(i - 1))
        });
        self.rebuild();
    }

    /// Pins as they were (undo inside a session): the mesh made again if
    /// they're at other places.
    pub fn set_pins(&mut self, pins: Vec<Pin>) {
        let same_places = pins.len() == self.pins.len()
            && pins.iter().zip(&self.pins).all(|(a, b)| a.rest == b.rest);
        if same_places {
            for (new, old) in pins.into_iter().zip(self.pins.iter_mut()) {
                let point = old.point;
                *old = Pin { point, ..new };
            }
            self.reorder();
        } else {
            self.pins = pins;
            self.rebuild();
        }
    }

    /// Pins and the bend as they were (undo inside a session): exactly, when
    /// the mesh is the same; else bent again from them.
    pub fn restore(&mut self, pins: Vec<Pin>, pos: Vec<[f32; 2]>) -> bool {
        self.set_pins(pins);
        if pos.len() == self.pos.len() {
            self.pos = pos;
            true
        } else {
            false
        }
    }

    /// Change pin `i` (where it's held, its turn, its depth): not where it
    /// is on the part.
    pub fn edit_pin(&mut self, i: usize, edit: impl FnOnce(&mut Pin)) {
        let Some(pin) = self.pins.get_mut(i) else {
            return;
        };
        let (rest, point, depth) = (pin.rest, pin.point, pin.depth);
        edit(pin);
        (pin.rest, pin.point) = (rest, point);
        if pin.depth != depth {
            self.reorder();
        }
    }

    /// The part's shape changed (finer or coarser, grown or shrunk): the
    /// mesh made again, the pins kept.
    pub fn set_shape(&mut self, shape: PuppetShape) {
        self.shape = shape;
        self.rebuild();
    }

    /// The mesh made again for the pins as they are, carrying the bend
    /// over: each new point starts where the old mesh had bent that spot.
    fn rebuild(&mut self) {
        if self.fixed.is_some() {
            let mut taken = std::collections::HashSet::new();
            let rest = &self.mesh.rest;
            self.pins.retain_mut(|pin| {
                pin.point = nearest(rest, pin.rest);
                taken.insert(pin.point)
            });
            self.solver = None;
            self.reorder();
            return;
        }
        let rests: Vec<[f32; 2]> = self.pins.iter().map(|p| p.rest).collect();
        let Some((mesh, places)) = self.shape.mesh(&rests) else {
            return;
        };
        let found = locate(&self.mesh.rest, &self.mesh.tris, &mesh.rest);
        let pos: Vec<[f32; 2]> = mesh
            .rest
            .iter()
            .zip(found)
            .map(|(&r, f)| match f {
                Some((t, w)) => {
                    let q = self.mesh.tris[t].map(|k| self.pos[k as usize]);
                    [
                        q[0][0] * w[0] + q[1][0] * w[1] + q[2][0] * w[2],
                        q[0][1] * w[0] + q[1][1] * w[1] + q[2][1] * w[2],
                    ]
                }
                None => r,
            })
            .collect();
        let mut taken = std::collections::HashSet::new();
        let mut kept = Vec::new();
        for (mut pin, place) in self.pins.drain(..).zip(places) {
            let point = place.unwrap_or_else(|| nearest(&mesh.rest, pin.rest));
            if taken.insert(point) {
                pin.point = point;
                kept.push(pin);
            }
        }
        self.pins = kept;
        self.nbrs = mesh.neighbours();
        self.held = vec![false; mesh.rest.len()];
        self.pos = pos;
        self.mesh = mesh;
        self.solver = None;
        self.reorder();
    }

    /// The drawing order: each triangle goes with the pin nearest it along
    /// the mesh, deeper pins' triangles last (on top).
    fn reorder(&mut self) {
        let n = self.mesh.rest.len();
        let mut owner = vec![usize::MAX; n];
        if self.pins.iter().any(|p| p.depth != 0) {
            // Nearest pin along the mesh (Dijkstra from all pins at once).
            let mut dist = vec![f32::INFINITY; n];
            let mut heap = std::collections::BinaryHeap::new();
            for (i, pin) in self.pins.iter().enumerate() {
                dist[pin.point] = 0.0;
                owner[pin.point] = i;
                heap.push((std::cmp::Reverse(Ord(0.0)), pin.point));
            }
            while let Some((std::cmp::Reverse(Ord(d)), v)) = heap.pop() {
                if d > dist[v] {
                    continue;
                }
                for &u in &self.nbrs[v] {
                    let u = u as usize;
                    let (a, b) = (self.mesh.rest[v], self.mesh.rest[u]);
                    let nd = d + ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
                    if nd < dist[u] {
                        dist[u] = nd;
                        owner[u] = owner[v];
                        heap.push((std::cmp::Reverse(Ord(nd)), u));
                    }
                }
            }
        }
        let depth = |t: &[u32; 3]| {
            t.iter()
                .filter_map(|&k| self.pins.get(owner[k as usize]).map(|p| p.depth))
                .max()
                .unwrap_or(0)
        };
        let mut order: Vec<u32> = (0..self.mesh.tris.len() as u32).collect();
        order.sort_by_key(|&t| depth(&self.mesh.tris[t as usize]));
        self.order = order;
    }

    /// The turn the mesh has at pin `i` now (radians): where turning it
    /// starts from.
    pub fn turn_now(&self, i: usize) -> f32 {
        let Some(pin) = self.pins.get(i) else {
            return 0.0;
        };
        if let Some(t) = pin.turn {
            return t;
        }
        // From its neighbours, rest to now.
        let (r0, p0) = (self.mesh.rest[pin.point], self.pos[pin.point]);
        let (mut a, mut b) = (0.0f32, 0.0f32);
        for &j in &self.nbrs[pin.point] {
            let (r, p) = (self.mesh.rest[j as usize], self.pos[j as usize]);
            let e = [r[0] - r0[0], r[1] - r0[1]];
            let f = [p[0] - p0[0], p[1] - p0[1]];
            a += e[0] * f[0] + e[1] * f[1];
            b += e[0] * f[1] - e[1] * f[0];
        }
        b.atan2(a)
    }

    /// Whether nothing has moved.
    pub fn is_identity(&self) -> bool {
        self.pos
            .iter()
            .zip(&self.mesh.rest)
            .all(|(p, r)| (p[0] - r[0]).abs() < 1e-3 && (p[1] - r[1]).abs() < 1e-3)
    }

    /// Rounds of bending a frame takes while a pin is dragged (it carries
    /// on from where the last frame got to).
    pub const FRAME_ROUNDS: usize = 4;

    /// Bend the mesh towards the pins: a frame's worth of rounds. Returns
    /// how far (pixels) it was still moving.
    pub fn solve(&mut self, mode: PuppetMode) -> f32 {
        self.bend(mode, Self::FRAME_ROUNDS)
    }

    /// Bend the mesh to the pins until it stops moving (a turn takes a
    /// while to spread down a long part).
    pub fn settle(&mut self, mode: PuppetMode) {
        for _ in 0..100 {
            if self.bend(mode, Self::FRAME_ROUNDS) < 0.01 {
                return;
            }
        }
    }

    /// `rounds` of bending; how far any point moved in the last.
    fn bend(&mut self, mode: PuppetMode, rounds: usize) -> f32 {
        let n = self.mesh.rest.len();
        // No pin moved or turned: exactly at rest (not to within float
        // error, so the paint comes back as it was).
        if self.pins.iter().all(|p| p.at == p.rest && p.turn.is_none()) {
            self.pos.clone_from(&self.mesh.rest);
            return 0.0;
        }
        // What's held, and where.
        let mut held = vec![false; n];
        let mut pinned_piece = vec![false; self.mesh.pieces];
        for pin in &self.pins {
            held[pin.point] = true;
            self.pos[pin.point] = pin.at;
            pinned_piece[self.mesh.piece[pin.point] as usize] = true;
        }
        for pin in &self.pins {
            let Some(turn) = pin.turn else {
                continue;
            };
            let (c, s) = (turn.cos(), turn.sin());
            let r0 = self.mesh.rest[pin.point];
            for &j in &self.nbrs[pin.point] {
                let j = j as usize;
                if held[j] && self.pins.iter().any(|p| p.point == j) {
                    continue;
                }
                let e = [self.mesh.rest[j][0] - r0[0], self.mesh.rest[j][1] - r0[1]];
                held[j] = true;
                self.pos[j] = [
                    pin.at[0] + c * e[0] - s * e[1],
                    pin.at[1] + s * e[0] + c * e[1],
                ];
            }
        }
        for p in 0..n {
            if !pinned_piece[self.mesh.piece[p] as usize] {
                held[p] = true;
                self.pos[p] = self.fixed.as_ref().map_or(self.mesh.rest[p], |s| s[p]);
            }
        }
        if self.solver.is_none() || held != self.held {
            self.solver = Solver::new(self.nbrs.clone(), held.clone());
            self.held = held;
        }
        match self.solver.as_mut() {
            Some(solver) => solver.solve(&self.mesh.rest, &mut self.pos, mode, rounds),
            None => 0.0,
        }
    }
}

/// An `f32` that sorts (distances, never NaN here).
#[derive(Clone, Copy, PartialEq)]
struct Ord(f32);
impl Eq for Ord {}
impl PartialOrd for Ord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(std::cmp::Ord::cmp(self, other))
    }
}
impl std::cmp::Ord for Ord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// The point of `points` nearest `p`.
fn nearest(points: &[[f32; 2]], p: [f32; 2]) -> usize {
    let d2 = |a: [f32; 2]| (a[0] - p[0]).powi(2) + (a[1] - p[1]).powi(2);
    (0..points.len())
        .min_by(|&a, &b| d2(points[a]).total_cmp(&d2(points[b])))
        .unwrap_or(0)
}

/// For each of `queries`, the triangle of `tris` (points at `verts`) it's
/// in and its weights there; triangles are binned so each query looks at a
/// few.
fn locate(
    verts: &[[f32; 2]],
    tris: &[[u32; 3]],
    queries: &[[f32; 2]],
) -> Vec<Option<(usize, [f32; 3])>> {
    if tris.is_empty() {
        return vec![None; queries.len()];
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for v in verts {
        (x0, y0, x1, y1) = (x0.min(v[0]), y0.min(v[1]), x1.max(v[0]), y1.max(v[1]));
    }
    // About one triangle per bin.
    let side = (((x1 - x0) * (y1 - y0)).max(1.0) / tris.len() as f32)
        .sqrt()
        .max(1.0);
    let (bw, bh) = (
        ((x1 - x0) / side) as usize + 1,
        ((y1 - y0) / side) as usize + 1,
    );
    let bin = |x: f32, y: f32| {
        (
            (((x - x0) / side) as usize).min(bw - 1),
            (((y - y0) / side) as usize).min(bh - 1),
        )
    };
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); bw * bh];
    for (i, t) in tris.iter().enumerate() {
        let p = t.map(|k| verts[k as usize]);
        let (a, b) = bin(
            p[0][0].min(p[1][0]).min(p[2][0]),
            p[0][1].min(p[1][1]).min(p[2][1]),
        );
        let (c, d) = bin(
            p[0][0].max(p[1][0]).max(p[2][0]),
            p[0][1].max(p[1][1]).max(p[2][1]),
        );
        for by in b..=d {
            for bx in a..=c {
                bins[by * bw + bx].push(i as u32);
            }
        }
    }
    queries
        .iter()
        .map(|&q| {
            if q[0] < x0 || q[1] < y0 || q[0] > x1 || q[1] > y1 {
                return None;
            }
            let (bx, by) = bin(q[0], q[1]);
            bins[by * bw + bx].iter().find_map(|&t| {
                let p = tris[t as usize].map(|k| verts[k as usize]);
                weights(p, q).map(|w| (t as usize, w))
            })
        })
        .collect()
}

/// `q`'s weights in triangle `p`, if it's inside (edges included).
fn weights(p: [[f32; 2]; 3], q: [f32; 2]) -> Option<[f32; 3]> {
    let area =
        (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
    if area.abs() < 1e-9 {
        return None;
    }
    let w0 = ((p[1][0] - q[0]) * (p[2][1] - q[1]) - (p[1][1] - q[1]) * (p[2][0] - q[0])) / area;
    let w1 = ((p[2][0] - q[0]) * (p[0][1] - q[1]) - (p[2][1] - q[1]) * (p[0][0] - q[0])) / area;
    let w = [w0, w1, 1.0 - w0 - w1];
    w.iter().all(|&v| v >= -1e-4).then_some(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(w: i32, h: i32, painted: impl Fn(i32, i32) -> bool) -> PuppetShape {
        PuppetShape::from_paint([0, 0, w, h], 10.0, 0, painted).unwrap()
    }

    fn bar() -> Puppet {
        Puppet::new(shape(200, 40, |_, _| true)).unwrap()
    }

    #[test]
    fn a_pin_sits_exactly_where_it_was_placed() {
        let mut p = bar();
        assert!(p.add_pin([500.0, 500.0]).is_none(), "off the part");
        let i = p.add_pin([23.3, 17.6]).unwrap();
        let point = p.pins()[i].point;
        assert!((p.mesh.rest[point][0] - 23.3).abs() < 1e-3);
        assert!((p.mesh.rest[point][1] - 17.6).abs() < 1e-3);
        assert_eq!(p.add_pin([23.4, 17.5]), Some(i), "the same pin");
    }

    #[test]
    fn a_pin_placed_on_the_bent_part_lands_on_the_paint_under_it() {
        let mut p = bar();
        let a = p.add_pin([10.0, 20.0]).unwrap();
        let b = p.add_pin([190.0, 20.0]).unwrap();
        p.edit_pin(b, |pin| pin.at[1] += 40.0);
        p.settle(PuppetMode::Rigid);
        // Where the bar's (150, 20) went, and a pin placed there.
        let mid = p.add_pin([10.0, 20.0]);
        assert_eq!(mid, Some(a));
        let spot = {
            let (t, w) = locate(&p.mesh.rest, &p.mesh.tris, &[[150.0, 20.0]])[0].unwrap();
            let q = p.mesh.tris[t].map(|k| p.pos[k as usize]);
            [
                q[0][0] * w[0] + q[1][0] * w[1] + q[2][0] * w[2],
                q[0][1] * w[0] + q[1][1] * w[1] + q[2][1] * w[2],
            ]
        };
        let c = p.add_pin(spot).unwrap();
        assert!((p.pins()[c].rest[0] - 150.0).abs() < 0.5);
        assert!((p.pins()[c].rest[1] - 20.0).abs() < 0.5);
        // Adding it didn't move anything much.
        let at = p.pos[p.pins()[c].point];
        assert!((at[0] - spot[0]).abs() < 0.5 && (at[1] - spot[1]).abs() < 0.5);
    }

    #[test]
    fn a_turned_pin_turns_the_paint_round_it() {
        let mut p = bar();
        let i = p.add_pin([100.0, 20.0]).unwrap();
        p.edit_pin(i, |pin| pin.turn = Some(0.5));
        p.settle(PuppetMode::Rigid);
        // The bar's far end swung round the pin (down, as y points down).
        let end = p.mesh.point_at(&p.mesh.rest, [200.0, 20.0]).unwrap();
        assert!(
            p.pos[end][1] > p.mesh.rest[end][1] + 30.0,
            "{:?}",
            p.pos[end]
        );
        assert!((p.turn_now(i) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn an_unpinned_piece_stays_and_a_pinned_one_follows() {
        let mut p = Puppet::new(shape(300, 40, |x, _| !(100..200).contains(&x))).unwrap();
        assert_eq!(p.mesh.pieces, 2);
        let i = p.add_pin([50.0, 20.0]).unwrap();
        p.edit_pin(i, |pin| pin.at[1] += 25.0);
        p.settle(PuppetMode::Normal);
        let left = p.mesh.point_at(&p.mesh.rest, [10.0, 10.0]).unwrap();
        let right = p.mesh.point_at(&p.mesh.rest, [250.0, 10.0]).unwrap();
        assert!((p.pos[left][1] - p.mesh.rest[left][1] - 25.0).abs() < 0.1);
        assert_eq!(p.pos[right], p.mesh.rest[right]);
    }

    #[test]
    fn a_new_shape_keeps_the_pins_and_the_bend() {
        let mut p = bar();
        p.add_pin([0.0, 20.0]).unwrap();
        let b = p.add_pin([200.0, 20.0]).unwrap();
        p.edit_pin(b, |pin| pin.at[1] += 30.0);
        p.settle(PuppetMode::Rigid);
        let fine = PuppetShape::from_paint([0, 0, 200, 40], 5.0, 6, |_, _| true).unwrap();
        p.set_shape(fine);
        p.settle(PuppetMode::Rigid);
        assert_eq!(p.pins().len(), 2);
        let pin = p.pins()[1];
        assert!((p.pos[pin.point][1] - pin.at[1]).abs() < 1e-3);
        assert!(!p.is_identity());
    }

    #[test]
    fn removing_pins_and_putting_them_back() {
        let mut p = bar();
        for x in [20.0, 100.0, 180.0] {
            p.add_pin([x, 20.0]).unwrap();
        }
        let before = p.pins().to_vec();
        p.remove_pins(&[0, 2]);
        assert_eq!(p.pins().len(), 1);
        assert_eq!(p.pins()[0].rest, before[1].rest);
        p.set_pins(before.clone());
        assert_eq!(p.pins().len(), 3);
        for (a, b) in p.pins().iter().zip(&before) {
            assert_eq!(a.rest, b.rest);
        }
    }

    #[test]
    fn a_deeper_pins_part_is_drawn_last() {
        let mut p = bar();
        let a = p.add_pin([20.0, 20.0]).unwrap();
        let b = p.add_pin([180.0, 20.0]).unwrap();
        p.edit_pin(b, |pin| pin.depth = 1);
        // Where the first and last triangles drawn are, across.
        let ends = |p: &Puppet| {
            let x = |t: u32| {
                let t = p.mesh.tris[t as usize];
                t.iter().map(|&k| p.mesh.rest[k as usize][0]).sum::<f32>() / 3.0
            };
            (x(p.order()[0]), x(*p.order().last().unwrap()))
        };
        let (first, last) = ends(&p);
        assert!(last > 100.0, "the right end (deeper) on top");
        assert!(first < 100.0);
        p.edit_pin(a, |pin| pin.depth = 2);
        assert!(ends(&p).1 < 100.0, "now the left end");
    }
}

#[cfg(test)]
mod fixed_tests {
    use super::*;

    #[test]
    fn a_fixed_mesh_keeps_its_points_and_pins_snap_to_them() {
        let mesh = PuppetMesh::from_paint([0, 0, 200, 40], 10.0, |_, _| true).unwrap();
        let n = mesh.rest.len();
        // Already bent a little.
        let pos: Vec<[f32; 2]> = mesh
            .rest
            .iter()
            .map(|p| [p[0], p[1] + p[0] * 0.05])
            .collect();
        let mut p = Puppet::fixed(mesh.clone(), pos.clone()).unwrap();
        let i = p.add_pin([23.3, 17.6 + 23.3 * 0.05]).unwrap();
        let pin = p.pins()[i];
        assert_eq!(p.mesh.rest.len(), n, "not made again");
        assert_eq!(pin.rest, mesh.rest[pin.point]);
        assert_eq!(pin.at, pos[pin.point], "held where it is");
        // Pins where the mesh has them: nothing moves.
        p.pin_points(&[pin.point, 0]);
        assert_eq!(p.pins().len(), 2);
        let j = p.add_pin(p.pos[0]).unwrap();
        assert_eq!(p.pins()[j].point, 0, "the same pin");
        p.edit_pin(0, |q| q.at[1] += 20.0);
        p.settle(PuppetMode::Normal);
        assert!((p.pos[pin.point][1] - pos[pin.point][1] - 20.0).abs() < 1e-3);
        assert_eq!(p.mesh.rest.len(), n);
    }
}
