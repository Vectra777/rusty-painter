//! A vector layer's lines, and an index of where they are, so finding the
//! lines near a spot (to draw, erase or pick them) doesn't look at every
//! line: on a page of line art that's thousands of them.
//!
//! The index is built when first asked for and kept up to date by the
//! layer's edits: a grid of 256 px cells listing the lines whose box
//! reaches each, lines reaching too many cells kept aside, and the lines
//! changed since it was built checked one by one until there are enough to
//! build it again (from the boxes it keeps, without reading the points).

use super::{VectorItem, VectorStroke};
use std::sync::{Arc, OnceLock};

/// Grid cell side, pixels.
const CELL: i32 = 256;
/// A line reaching more cells than this is kept aside rather than in each.
const WIDE: i64 = 64;
/// Changed lines checked one by one before the grid is built again.
const LOOSE: usize = 256;

/// A vector layer's lines and shapes, bottom first. Copies share them (and
/// the index while neither changes): the copy an undo step keeps is cheap.
pub struct VectorLayer {
    items: Vec<Arc<VectorItem>>,
    index: OnceLock<Arc<Index>>,
    /// Changes with every edit (never the same for different lines): what
    /// a picture of them was drawn from, to tell it's out of date.
    version: u64,
}

impl Default for VectorLayer {
    fn default() -> Self {
        Self::from_items(Vec::new())
    }
}

/// The next [`VectorLayer::version`].
fn next_version() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Where the lines are: each one's box, and a grid over them.
#[derive(Clone, Default)]
struct Index {
    /// Each item's [`VectorItem::bounds`].
    bounds: Vec<[i32; 4]>,
    /// The grid's top-left cell and its size in cells.
    origin: [i32; 2],
    cols: i32,
    rows: i32,
    /// For cell `c`, the lines in it are `entries[starts[c]..starts[c + 1]]`.
    starts: Vec<u32>,
    entries: Vec<u32>,
    /// Lines too big for the grid, and those changed since it was built.
    wide: Vec<u32>,
    loose: Vec<u32>,
}

impl Index {
    fn new(items: &[Arc<VectorItem>]) -> Self {
        let mut index = Index {
            bounds: items.iter().map(|s| s.bounds()).collect(),
            ..Default::default()
        };
        index.build_grid();
        index
    }

    /// The grid again, from the boxes.
    fn build_grid(&mut self) {
        self.loose.clear();
        self.wide.clear();
        let cells = |b: &[i32; 4]| {
            [
                b[0].div_euclid(CELL),
                b[1].div_euclid(CELL),
                (b[2] - 1).div_euclid(CELL),
                (b[3] - 1).div_euclid(CELL),
            ]
        };
        let mut span = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for b in self.bounds.iter().filter(|b| b[0] < b[2] && b[1] < b[3]) {
            let c = cells(b);
            span = [
                span[0].min(c[0]),
                span[1].min(c[1]),
                span[2].max(c[2]),
                span[3].max(c[3]),
            ];
        }
        if span[0] > span[2] {
            *self = Index {
                bounds: std::mem::take(&mut self.bounds),
                ..Default::default()
            };
            return;
        }
        self.origin = [span[0], span[1]];
        self.cols = span[2] - span[0] + 1;
        self.rows = span[3] - span[1] + 1;
        // Counted, then placed: one pass each, no list per cell.
        let n = (self.cols * self.rows) as usize;
        let mut counts = vec![0u32; n + 1];
        let mut placed = Vec::with_capacity(self.bounds.len());
        for (i, b) in self.bounds.iter().enumerate() {
            if b[0] >= b[2] || b[1] >= b[3] {
                continue;
            }
            let c = cells(b);
            let area = i64::from(c[2] - c[0] + 1) * i64::from(c[3] - c[1] + 1);
            if area > WIDE {
                self.wide.push(i as u32);
                continue;
            }
            placed.push((i as u32, c));
            for y in c[1]..=c[3] {
                for x in c[0]..=c[2] {
                    counts[self.cell(x, y) + 1] += 1;
                }
            }
        }
        for c in 1..=n {
            counts[c] += counts[c - 1];
        }
        let mut next = counts.clone();
        self.entries = vec![0; counts[n] as usize];
        for (i, c) in placed {
            for y in c[1]..=c[3] {
                for x in c[0]..=c[2] {
                    let slot = &mut next[self.cell(x, y)];
                    self.entries[*slot as usize] = i;
                    *slot += 1;
                }
            }
        }
        self.starts = counts;
    }

    fn cell(&self, x: i32, y: i32) -> usize {
        ((y - self.origin[1]) * self.cols + (x - self.origin[0])) as usize
    }

    /// Line `i` changed (or is new): checked one by one until the grid is
    /// built again.
    fn touched(&mut self, i: usize) {
        if self.loose.last() != Some(&(i as u32)) {
            self.loose.push(i as u32);
        }
        if self.loose.len() > LOOSE {
            self.build_grid();
        }
    }

    /// The lines whose box meets `rect`, bottom first.
    fn candidates(&self, rect: [i32; 4]) -> Vec<usize> {
        let meets = |i: u32| {
            let b = &self.bounds[i as usize];
            b[0] < rect[2] && rect[0] < b[2] && b[1] < rect[3] && rect[1] < b[3]
        };
        let mut out: Vec<u32> = Vec::new();
        if self.cols > 0 && rect[0] < rect[2] && rect[1] < rect[3] {
            let x0 = rect[0].div_euclid(CELL).max(self.origin[0]);
            let y0 = rect[1].div_euclid(CELL).max(self.origin[1]);
            let x1 = (rect[2] - 1)
                .div_euclid(CELL)
                .min(self.origin[0] + self.cols - 1);
            let y1 = (rect[3] - 1)
                .div_euclid(CELL)
                .min(self.origin[1] + self.rows - 1);
            // A rect over most of the grid: the boxes are quicker to scan.
            let area = i64::from(x1 - x0 + 1) * i64::from(y1 - y0 + 1);
            if area > (self.bounds.len() as i64 / 4).max(16) {
                out.extend((0..self.bounds.len() as u32).filter(|&i| meets(i)));
                return out.into_iter().map(|i| i as usize).collect();
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let c = self.cell(x, y);
                    let cell = &self.entries[self.starts[c] as usize..self.starts[c + 1] as usize];
                    out.extend(cell.iter().copied().filter(|&i| meets(i)));
                }
            }
        }
        out.extend(
            self.wide
                .iter()
                .chain(&self.loose)
                .copied()
                .filter(|&i| meets(i)),
        );
        out.sort_unstable();
        out.dedup();
        out.into_iter().map(|i| i as usize).collect()
    }
}

impl VectorLayer {
    pub fn from_strokes(strokes: Vec<VectorStroke>) -> Self {
        Self::from_items(strokes.into_iter().map(VectorItem::Line).collect())
    }

    pub fn from_items(items: Vec<VectorItem>) -> Self {
        VectorLayer {
            items: items.into_iter().map(Arc::new).collect(),
            index: OnceLock::new(),
            version: next_version(),
        }
    }

    /// See the field: changes with every edit.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The lines and shapes, bottom first.
    pub fn items(&self) -> &[Arc<VectorItem>] {
        &self.items
    }

    /// Item `i`, if it's a brush line.
    pub fn line(&self, i: usize) -> Option<&VectorStroke> {
        self.items.get(i).and_then(|item| item.as_line())
    }

    /// The brush lines (not in shapes or groups).
    pub fn lines(&self) -> impl Iterator<Item = &VectorStroke> {
        self.items.iter().filter_map(|i| i.as_line())
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn index(&self) -> &Index {
        self.index.get_or_init(|| Arc::new(Index::new(&self.items)))
    }

    /// The index to change along with the items, if it's been built (one
    /// a copy shares is copied first).
    fn index_mut(&mut self) -> Option<&mut Index> {
        self.index.get_mut().map(Arc::make_mut)
    }

    /// Item `i`'s box ([`VectorItem::bounds`]).
    pub fn bounds(&self, i: usize) -> [i32; 4] {
        self.index().bounds[i]
    }

    /// The box around every item, if there are any.
    pub fn union_bounds(&self) -> Option<[i32; 4]> {
        super::union(self.index().bounds.iter().copied().filter(|b| b[0] < b[2]))
    }

    /// The items whose box meets `rect` (`[x0, y0, x1, y1)`), bottom first.
    pub fn candidates(&self, rect: [i32; 4]) -> Vec<usize> {
        self.index().candidates(rect)
    }

    /// The topmost item within `tolerance` of `p`.
    pub fn pick(&self, p: eframe::egui::Vec2, tolerance: f32) -> Option<usize> {
        let r = tolerance.ceil() as i32 + 1;
        let (x, y) = (p.x.floor() as i32, p.y.floor() as i32);
        let near = self.candidates([x - r, y - r, x + r + 1, y + r + 1]);
        near.into_iter()
            .rev()
            .find(|&i| self.items[i].touches(p, tolerance))
    }

    /// A line on top.
    pub fn push(&mut self, stroke: VectorStroke) {
        self.push_item(VectorItem::Line(stroke));
    }

    /// An item on top.
    pub fn push_item(&mut self, item: VectorItem) {
        self.version = next_version();
        let bounds = item.bounds();
        self.items.push(Arc::new(item));
        let i = self.items.len() - 1;
        if let Some(index) = self.index_mut() {
            index.bounds.push(bounds);
            index.touched(i);
        }
    }

    /// A point on the end of the top line (the one being drawn).
    pub fn append_point(&mut self, point: [f32; 3]) {
        self.version = next_version();
        let Some(last) = self.items.last_mut() else {
            return;
        };
        let VectorItem::Line(line) = Arc::make_mut(last) else {
            return;
        };
        line.points.push(point);
        let i = self.items.len() - 1;
        if let Some(index) = self.index_mut() {
            let p = super::bounds_of_widths(&[point], 1.0);
            let b = &mut index.bounds[i];
            *b = [
                b[0].min(p[0]),
                b[1].min(p[1]),
                b[2].max(p[2]),
                b[3].max(p[3]),
            ];
            index.touched(i);
        }
    }

    /// Change item `i`, if it's a brush line.
    pub fn edit(&mut self, i: usize, edit: impl FnOnce(&mut VectorStroke)) {
        self.edit_item(i, |item| {
            if let VectorItem::Line(l) = item {
                edit(l);
            }
        });
    }

    /// Change item `i`.
    pub fn edit_item(&mut self, i: usize, edit: impl FnOnce(&mut VectorItem)) {
        self.version = next_version();
        let Some(item) = self.items.get_mut(i) else {
            return;
        };
        edit(Arc::make_mut(item));
        let bounds = item.bounds();
        if let Some(index) = self.index_mut() {
            index.bounds[i] = bounds;
            index.touched(i);
        }
    }

    /// Put each `(i, pieces)`'s pieces in place of item `i` (none takes it
    /// out), the indices ascending and different.
    pub fn splice(&mut self, edits: Vec<(usize, Vec<VectorItem>)>) {
        self.version = next_version();
        if edits.is_empty() {
            return;
        }
        let old = std::mem::take(&mut self.items);
        let mut bounds = self.index.get().map(|ix| ix.bounds.as_slice());
        let mut new_bounds = Vec::new();
        let mut edits = edits.into_iter().peekable();
        for (i, item) in old.into_iter().enumerate() {
            match edits.next_if(|(at, _)| *at == i) {
                Some((_, pieces)) => {
                    for piece in pieces {
                        new_bounds.push(piece.bounds());
                        self.items.push(Arc::new(piece));
                    }
                }
                None => {
                    if let Some(b) = bounds.as_mut() {
                        new_bounds.push(b[i]);
                    }
                    self.items.push(item);
                }
            }
        }
        // Built again from the boxes kept: the indices above moved.
        self.index = match bounds {
            Some(_) => {
                let mut index = Index {
                    bounds: new_bounds,
                    ..Default::default()
                };
                index.build_grid();
                OnceLock::from(Arc::new(index))
            }
            None => OnceLock::new(),
        };
    }

    /// Take out item `i`.
    pub fn remove(&mut self, i: usize) {
        self.splice(vec![(i, Vec::new())]);
    }

    /// Change every brush line (their points, widths or colour).
    pub fn edit_all(&mut self, mut edit: impl FnMut(&mut VectorStroke)) {
        self.edit_all_items(|item| {
            if let VectorItem::Line(l) = item {
                edit(l);
            }
        });
    }

    /// Change every item.
    pub fn edit_all_items(&mut self, mut edit: impl FnMut(&mut VectorItem)) {
        self.version = next_version();
        for item in &mut self.items {
            edit(Arc::make_mut(item));
        }
        self.index = OnceLock::new();
    }
}

impl Clone for VectorLayer {
    fn clone(&self) -> Self {
        let index = OnceLock::new();
        if let Some(ix) = self.index.get() {
            let _ = index.set(ix.clone());
        }
        VectorLayer {
            items: self.items.clone(),
            index,
            version: self.version,
        }
    }
}

impl PartialEq for VectorLayer {
    fn eq(&self, other: &Self) -> bool {
        self.items.len() == other.items.len()
            && (self.items.iter().zip(&other.items)).all(|(a, b)| Arc::ptr_eq(a, b) || a == b)
    }
}

impl std::fmt::Debug for VectorLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorLayer")
            .field("items", &self.items)
            .finish()
    }
}

/// Saved as `{"items": [...]}`; layers saved before shapes were kept as
/// `{"strokes": [...]}` (brush lines only), and load as such.
#[derive(serde::Serialize)]
struct Saving<'a> {
    items: Vec<&'a VectorItem>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Loading {
    Now { items: Vec<VectorItem> },
    Before { strokes: Vec<VectorStroke> },
}

impl serde::Serialize for VectorLayer {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Saving {
            items: self.items.iter().map(|s| &**s).collect(),
        }
        .serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for VectorLayer {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Loading::deserialize(d)? {
            Loading::Now { items } => VectorLayer::from_items(items),
            Loading::Before { strokes } => VectorLayer::from_strokes(strokes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Vec2;

    /// `n` lines of all sizes over a `size` square, some far off it.
    fn lines(n: usize, size: f32, seed: u32) -> Vec<VectorStroke> {
        let mut seed = seed;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        (0..n)
            .map(|i| {
                let start = Vec2::new(rand() - 0.1, rand() - 0.1) * size;
                // Mostly short, some across many cells.
                let len = if i % 13 == 0 {
                    size * rand()
                } else {
                    4.0 + rand() * 300.0
                };
                let dir = Vec2::angled(rand() * 6.3);
                let points = (0..=6)
                    .map(|k| {
                        let p = start + dir * len * (k as f32 / 6.0);
                        [p.x, p.y, 1.0 + rand() * 20.0]
                    })
                    .collect();
                VectorStroke {
                    points,
                    colour: [0; 3],
                    opacity: 1.0,
                }
            })
            .collect()
    }

    fn brute(layer: &VectorLayer, rect: [i32; 4]) -> Vec<usize> {
        (0..layer.len())
            .filter(|&i| {
                let b = layer.items()[i].bounds();
                b[0] < rect[2] && rect[0] < b[2] && b[1] < rect[3] && rect[1] < b[3]
            })
            .collect()
    }

    fn rects(n: usize, size: i32) -> Vec<[i32; 4]> {
        (0..n as i32)
            .map(|k| {
                let (x, y) = ((k * 977) % size - 100, (k * 613) % size - 100);
                let (w, h) = (1 + (k * 37) % 700, 1 + (k * 53) % 500);
                [x, y, x + w, y + h]
            })
            .collect()
    }

    #[test]
    fn the_index_finds_exactly_the_lines_a_rect_meets() {
        let layer = VectorLayer::from_strokes(lines(3000, 5000.0, 7));
        for rect in rects(300, 5200) {
            assert_eq!(layer.candidates(rect), brute(&layer, rect), "{rect:?}");
        }
    }

    #[test]
    fn edits_keep_the_index_right() {
        let mut layer = VectorLayer::from_strokes(lines(800, 3000.0, 11));
        let _ = layer.candidates([0, 0, 1, 1]);
        let more = lines(600, 3000.0, 23);
        for (k, s) in more.into_iter().enumerate() {
            match k % 4 {
                // Past the loose list's length too: the grid is built again.
                0 => layer.push(s),
                1 => layer.append_point([k as f32 * 5.0, 2900.0 - k as f32, 9.0]),
                2 => {
                    let i = (k * 31) % layer.len();
                    layer.edit(i, |l| l.points = s.points.clone());
                }
                _ => {
                    let (a, b) = ((k * 7) % layer.len(), (k * 13) % layer.len());
                    let (a, b) = (a.min(b), a.max(b));
                    let mut edits = vec![(a, vec![s.clone(), s.clone()])];
                    if b != a {
                        edits.push((b, Vec::new()));
                    }
                    layer.splice(
                        edits
                            .into_iter()
                            .map(|(i, p)| (i, p.into_iter().map(VectorItem::Line).collect()))
                            .collect(),
                    );
                }
            }
            if k % 50 == 0 {
                for rect in rects(40, 3200) {
                    assert_eq!(layer.candidates(rect), brute(&layer, rect), "after {k}");
                }
            }
        }
        // A copy and the original change apart.
        let copy = layer.clone();
        layer.remove(0);
        assert_eq!(copy.len(), layer.len() + 1);
        for rect in rects(40, 3200) {
            assert_eq!(copy.candidates(rect), brute(&copy, rect));
            assert_eq!(layer.candidates(rect), brute(&layer, rect));
        }
    }

    #[test]
    fn layers_saved_before_shapes_load_as_lines() {
        let strokes = lines(3, 100.0, 5);
        let before = serde_json::json!({ "strokes": strokes });
        let layer: VectorLayer = serde_json::from_value(before).unwrap();
        assert_eq!(layer, VectorLayer::from_strokes(strokes));
        let json = serde_json::to_value(&layer).unwrap();
        assert_eq!(
            json.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["items"]
        );
        let back: VectorLayer = serde_json::from_value(json).unwrap();
        assert_eq!(back, layer);
    }
}
