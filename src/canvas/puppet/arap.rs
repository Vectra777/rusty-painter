//! Bending a mesh by some of its points (pins), keeping the rest of it as
//! rigid as it can: "as-rigid-as-possible" deformation (Sorkine and Alexa,
//! 2007), in 2D. Each point gets the turn (and, softer modes, the size
//! change) that best fits its neighbours, then every free point is placed
//! where those turns ask for at once (one linear solve, the same matrix
//! every time); a few rounds of the two settle it.
//!
//! The matrix (the mesh's graph Laplacian over the free points) only
//! changes when which points are pinned changes: it's factored then
//! (Cholesky, banded after a reverse Cuthill–McKee ordering, which keeps a
//! grid mesh's band narrow), and every drag frame is back-substitutions.

/// How freely the mesh may change shape between pins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PuppetMode {
    /// Each part only turns: lengths kept.
    Rigid,
    /// Parts turn and may grow or shrink a little.
    #[default]
    Normal,
    /// Parts stretch freely: a smooth spread of the pins' moves.
    Distort,
}

impl PuppetMode {
    pub const ALL: [(PuppetMode, &'static str); 3] = [
        (PuppetMode::Rigid, "Rigid"),
        (PuppetMode::Normal, "Normal"),
        (PuppetMode::Distort, "Distort"),
    ];
}

/// The factored system for one set of pinned points.
pub struct Solver {
    /// Each point's neighbours.
    nbrs: Vec<Vec<u32>>,
    /// Pinned points (placed by the caller, not solved).
    fixed: Vec<bool>,
    /// A free point's row in the system, `u32::MAX` for a fixed one.
    row: Vec<u32>,
    /// Row → point.
    point: Vec<u32>,
    chol: BandCholesky,
    /// Each point's best fit turn and scale as `(c, s)`: the matrix
    /// `[[c, −s], [s, c]]`.
    fit: Vec<[f64; 2]>,
}

impl Solver {
    /// The system for `nbrs` (each point's neighbours) with the points
    /// `fixed` pinned. Every piece of the mesh must have a fixed point
    /// (else it has nothing to hold it): `None` otherwise.
    pub fn new(nbrs: Vec<Vec<u32>>, fixed: Vec<bool>) -> Option<Solver> {
        let n = nbrs.len();
        // Free points, in reverse Cuthill–McKee order.
        let order = rcm(&nbrs, &fixed);
        let mut row = vec![u32::MAX; n];
        for (r, &p) in order.iter().enumerate() {
            row[p as usize] = r as u32;
        }
        let m = order.len();
        let mut band = 0;
        for &p in &order {
            for &q in &nbrs[p as usize] {
                if row[q as usize] != u32::MAX {
                    band = band.max(row[p as usize].abs_diff(row[q as usize]) as usize);
                }
            }
        }
        let mut chol = BandCholesky::zeros(m, band);
        for &p in &order {
            let r = row[p as usize] as usize;
            chol.set(r, r, nbrs[p as usize].len() as f64);
            for &q in &nbrs[p as usize] {
                let c = row[q as usize];
                if c != u32::MAX && (c as usize) < r {
                    chol.set(r, c as usize, -1.0);
                }
            }
        }
        chol.factor()?;
        Some(Solver {
            nbrs,
            fixed,
            row,
            point: order,
            chol,
            fit: vec![[1.0, 0.0]; n],
        })
    }

    pub fn is_fixed(&self, p: usize) -> bool {
        self.fixed[p]
    }

    /// Point `p`'s turn (radians) at the last solve.
    pub fn turn_at(&self, p: usize) -> f32 {
        let [c, s] = self.fit[p];
        s.atan2(c) as f32
    }

    /// Bend the mesh `rounds` times over: `rest` its points at rest,
    /// `pos` where they are (the fixed ones already where the pins put
    /// them; the others are the starting guess and get the result). Each
    /// round places the free points for the turns fitted last time, then
    /// fits the turns again. Returns how far (pixels) any point moved in
    /// the last round: near nothing once it has settled.
    pub fn solve(
        &mut self,
        rest: &[[f32; 2]],
        pos: &mut [[f32; 2]],
        mode: PuppetMode,
        rounds: usize,
    ) -> f32 {
        let n = rest.len();
        let r: Vec<[f64; 2]> = rest.iter().map(|p| [p[0] as f64, p[1] as f64]).collect();
        let mut p: Vec<[f64; 2]> = pos.iter().map(|p| [p[0] as f64, p[1] as f64]).collect();
        let m = self.point.len();
        let (mut bx, mut by) = (vec![0.0; m], vec![0.0; m]);
        let mut moved = 0.0f64;
        if mode == PuppetMode::Distort {
            self.fit.fill([1.0, 0.0]);
        }
        for _ in 0..rounds.max(1) {
            // Global: the free points where the turns put them.
            for (k, &pi) in self.point.iter().enumerate() {
                let i = pi as usize;
                let (mut sx, mut sy) = (0.0, 0.0);
                for &j in &self.nbrs[i] {
                    let j = j as usize;
                    let c = 0.5 * (self.fit[i][0] + self.fit[j][0]);
                    let s = 0.5 * (self.fit[i][1] + self.fit[j][1]);
                    let e = [r[i][0] - r[j][0], r[i][1] - r[j][1]];
                    sx += c * e[0] - s * e[1];
                    sy += s * e[0] + c * e[1];
                    if self.row[j] == u32::MAX {
                        sx += p[j][0];
                        sy += p[j][1];
                    }
                }
                bx[k] = sx;
                by[k] = sy;
            }
            self.chol.solve(&mut bx, &mut by);
            moved = 0.0;
            for (k, &pi) in self.point.iter().enumerate() {
                let q = &mut p[pi as usize];
                moved = moved.max((q[0] - bx[k]).abs().max((q[1] - by[k]).abs()));
                *q = [bx[k], by[k]];
            }
            if mode == PuppetMode::Distort {
                // Nothing to fit: one placing is the answer.
                break;
            }
            // Local: each point's best turn (and scale) for its neighbours.
            for i in 0..n {
                let (mut a, mut b, mut len) = (0.0, 0.0, 0.0);
                for &j in &self.nbrs[i] {
                    let j = j as usize;
                    let e = [r[i][0] - r[j][0], r[i][1] - r[j][1]];
                    let f = [p[i][0] - p[j][0], p[i][1] - p[j][1]];
                    a += e[0] * f[0] + e[1] * f[1];
                    b += e[0] * f[1] - e[1] * f[0];
                    len += e[0] * e[0] + e[1] * e[1];
                }
                let norm = (a * a + b * b).sqrt();
                self.fit[i] = if norm < 1e-12 || len < 1e-12 {
                    [1.0, 0.0]
                } else {
                    let scale = match mode {
                        PuppetMode::Rigid => 1.0,
                        // Half way (in ratio) to the best size.
                        _ => (norm / len).sqrt(),
                    };
                    [a / norm * scale, b / norm * scale]
                };
            }
        }
        for &pi in &self.point {
            let pi = pi as usize;
            pos[pi] = [p[pi][0] as f32, p[pi][1] as f32];
        }
        moved as f32
    }
}

/// The free points in reverse Cuthill–McKee order: each piece breadth
/// first from a point of fewest neighbours, neighbours fewest first, all
/// reversed.
fn rcm(nbrs: &[Vec<u32>], fixed: &[bool]) -> Vec<u32> {
    let n = nbrs.len();
    let free_degree = |p: usize| nbrs[p].iter().filter(|&&q| !fixed[q as usize]).count();
    let mut by_degree: Vec<usize> = (0..n).filter(|&p| !fixed[p]).collect();
    by_degree.sort_by_key(|&p| free_degree(p));
    let mut seen = vec![false; n];
    let mut order = Vec::with_capacity(by_degree.len());
    for &start in &by_degree {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut head = order.len();
        order.push(start as u32);
        while head < order.len() {
            let p = order[head] as usize;
            head += 1;
            let mut next: Vec<usize> = (nbrs[p].iter())
                .map(|&q| q as usize)
                .filter(|&q| !fixed[q] && !seen[q])
                .collect();
            next.sort_by_key(|&q| free_degree(q));
            for q in next {
                seen[q] = true;
                order.push(q as u32);
            }
        }
    }
    order.reverse();
    order
}

/// A symmetric positive definite banded matrix, factored in place as
/// `L·Lᵀ` (its lower band kept, row by row).
struct BandCholesky {
    n: usize,
    band: usize,
    /// Row `i`'s entries `i − band ..= i`, at `i * (band + 1) + (j + band − i)`.
    data: Vec<f64>,
}

impl BandCholesky {
    fn zeros(n: usize, band: usize) -> Self {
        Self {
            n,
            band,
            data: vec![0.0; n * (band + 1)],
        }
    }

    #[inline]
    fn at(&self, i: usize, j: usize) -> usize {
        i * (self.band + 1) + (j + self.band - i)
    }

    /// Entry `(i, j)`, `j ≤ i`, within the band.
    fn set(&mut self, i: usize, j: usize, v: f64) {
        let k = self.at(i, j);
        self.data[k] = v;
    }

    /// Factor; `None` if the matrix isn't positive definite.
    fn factor(&mut self) -> Option<()> {
        let (n, band) = (self.n, self.band);
        for i in 0..n {
            let lo = i.saturating_sub(band);
            for j in lo..=i {
                let mut sum = self.data[self.at(i, j)];
                for k in lo.max(j.saturating_sub(band))..j {
                    sum -= self.data[self.at(i, k)] * self.data[self.at(j, k)];
                }
                if i == j {
                    if sum <= 1e-12 {
                        return None;
                    }
                    let k = self.at(i, i);
                    self.data[k] = sum.sqrt();
                } else {
                    let k = self.at(i, j);
                    self.data[k] = sum / self.data[self.at(j, j)];
                }
            }
        }
        Some(())
    }

    /// `x` ← A⁻¹·`x` and `y` ← A⁻¹·`y`, both in one pass over the factor
    /// (each row read once, front to back).
    fn solve(&self, x: &mut [f64], y: &mut [f64]) {
        let (n, band) = (self.n, self.band);
        let w = band + 1;
        // L·z = b, row by row.
        for i in 0..n {
            let lo = i.saturating_sub(band);
            let row = &self.data[i * w + lo + band - i..i * w + band];
            let (mut sx, mut sy) = (0.0, 0.0);
            for ((l, a), b) in row.iter().zip(&x[lo..i]).zip(&y[lo..i]) {
                sx += l * a;
                sy += l * b;
            }
            let d = self.data[i * w + band];
            x[i] = (x[i] - sx) / d;
            y[i] = (y[i] - sy) / d;
        }
        // Lᵀ·out = z, from the end: each finished entry taken out of the
        // ones before it along its row.
        for i in (0..n).rev() {
            let d = self.data[i * w + band];
            x[i] /= d;
            y[i] /= d;
            let lo = i.saturating_sub(band);
            let row = &self.data[i * w + lo + band - i..i * w + band];
            let (xi, yi) = (x[i], y[i]);
            for ((l, a), b) in row.iter().zip(&mut x[lo..i]).zip(&mut y[lo..i]) {
                *a -= l * xi;
                *b -= l * yi;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::puppet::mesh::PuppetMesh;

    fn bar() -> PuppetMesh {
        // A 200×40 bar.
        PuppetMesh::from_paint([0, 0, 200, 40], 10.0, |_, _| true).unwrap()
    }

    fn nearest(m: &PuppetMesh, p: [f32; 2]) -> usize {
        m.point_at(&m.rest, p).unwrap()
    }

    fn settle(s: &mut Solver, m: &PuppetMesh, pos: &mut [[f32; 2]], mode: PuppetMode) {
        for _ in 0..100 {
            if s.solve(&m.rest, pos, mode, 8) < 1e-3 {
                return;
            }
        }
        panic!("didn't settle");
    }

    fn edge_lengths(m: &PuppetMesh, pos: &[[f32; 2]]) -> Vec<f32> {
        m.tris
            .iter()
            .flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
            .map(|(a, b)| {
                let (a, b) = (pos[a as usize], pos[b as usize]);
                ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
            })
            .collect()
    }

    #[test]
    fn the_band_solve_matches_the_matrix() {
        // A small SPD tridiagonal-plus system, against a direct check.
        let mut c = BandCholesky::zeros(4, 1);
        for i in 0..4 {
            c.set(i, i, 4.0);
            if i > 0 {
                c.set(i, i - 1, -1.0);
            }
        }
        c.factor().unwrap();
        let mut x = vec![1.0, 2.0, 3.0, 4.0];
        c.solve(&mut x, &mut [0.0; 4]);
        let a = |i: usize, j: usize| {
            if i == j {
                4.0
            } else if i.abs_diff(j) == 1 {
                -1.0
            } else {
                0.0
            }
        };
        for (i, want) in [1.0, 2.0, 3.0, 4.0].iter().enumerate() {
            let got: f64 = (0..4).map(|j| a(i, j) * x[j]).sum();
            assert!((got - want).abs() < 1e-9);
        }
    }

    #[test]
    fn pins_left_at_rest_change_nothing() {
        let m = bar();
        let mut fixed = vec![false; m.rest.len()];
        fixed[nearest(&m, [0.0, 0.0])] = true;
        fixed[nearest(&m, [200.0, 40.0])] = true;
        let mut s = Solver::new(m.neighbours(), fixed).unwrap();
        for mode in [PuppetMode::Rigid, PuppetMode::Normal, PuppetMode::Distort] {
            let mut pos = m.rest.clone();
            s.solve(&m.rest, &mut pos, mode, 8);
            for (a, b) in pos.iter().zip(&m.rest) {
                assert!(
                    (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3,
                    "{mode:?}"
                );
            }
        }
    }

    #[test]
    fn one_pin_carries_its_piece_along() {
        let m = bar();
        let pin = nearest(&m, [100.0, 20.0]);
        let mut fixed = vec![false; m.rest.len()];
        fixed[pin] = true;
        let mut s = Solver::new(m.neighbours(), fixed).unwrap();
        let mut pos = m.rest.clone();
        pos[pin][0] += 30.0;
        pos[pin][1] -= 10.0;
        settle(&mut s, &m, &mut pos, PuppetMode::Rigid);
        for (a, b) in pos.iter().zip(&m.rest) {
            assert!((a[0] - b[0] - 30.0).abs() < 0.05 && (a[1] - b[1] + 10.0).abs() < 0.05);
        }
    }

    #[test]
    fn bending_the_bar_keeps_it_rigid_and_the_pins_where_they_were_put() {
        let m = bar();
        let (left, mid, right) = (
            nearest(&m, [0.0, 20.0]),
            nearest(&m, [100.0, 20.0]),
            nearest(&m, [200.0, 20.0]),
        );
        let mut fixed = vec![false; m.rest.len()];
        for p in [left, mid, right] {
            fixed[p] = true;
        }
        let mut s = Solver::new(m.neighbours(), fixed).unwrap();
        let mut pos = m.rest.clone();
        // The right end swung up round the middle.
        let r = m.rest[right];
        let (c, sn) = (0.6f32.cos(), (-0.6f32).sin());
        let d = [r[0] - m.rest[mid][0], r[1] - m.rest[mid][1]];
        pos[right] = [
            m.rest[mid][0] + c * d[0] - sn * d[1],
            m.rest[mid][1] + sn * d[0] + c * d[1],
        ];
        let target = pos[right];
        settle(&mut s, &m, &mut pos, PuppetMode::Rigid);
        assert_eq!(pos[right], target, "a pin stays put");
        assert_eq!(pos[left], m.rest[left]);
        let (before, after) = (edge_lengths(&m, &m.rest), edge_lengths(&m, &pos));
        let worst = before
            .iter()
            .zip(&after)
            .map(|(a, b)| (a - b).abs() / a)
            .fold(0.0, f32::max);
        assert!(worst < 0.25, "edges stretched by {worst}");
        // The free end of the bar's right half turned with it.
        let tip = nearest(&m, [190.0, 0.0]);
        assert!(pos[tip][1] < m.rest[tip][1] - 20.0, "{:?}", pos[tip]);
        // Distort spreads the move smoothly but bends more.
        let mut soft = m.rest.clone();
        soft[right] = target;
        settle(&mut s, &m, &mut soft, PuppetMode::Distort);
        let soft_worst = before
            .iter()
            .zip(edge_lengths(&m, &soft))
            .map(|(a, b)| (a - b).abs() / a)
            .fold(0.0, f32::max);
        assert!(soft_worst > worst, "distort {soft_worst} vs rigid {worst}");
    }

    #[test]
    fn a_piece_with_nothing_pinned_has_no_solution() {
        let m = bar();
        assert!(Solver::new(m.neighbours(), vec![false; m.rest.len()]).is_none());
    }
}
