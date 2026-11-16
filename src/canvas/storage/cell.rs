//! A tile's storage: its 8-bit pixels and, in a deeper document, the same
//! pixels at full depth (see [`super::deep`]). The fields are private so
//! that every write to the 8-bit pixels goes through here, which drops the
//! deep pixels it would leave stale; full-depth writers use
//! [`TileCell::deep_parts`] and keep both.
//!
//! The pixels are shared copy-on-write: a snapshot (undo, a save's copy of
//! the canvas) takes the tile's pixels as they are, for nothing, and the
//! tile's next write copies them first if anything still shares them.
//!
//! Every write also gives the tile a new revision, unique across all
//! tiles, so what's made from its pixels (a styled layer's border) can be
//! kept until they change.

use super::deep::{DeepTile, Depth};
use eframe::egui::Color32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The next tile revision (0 is never given: it stands for no tile).
fn next_revision() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A tile's 8-bit pixels, shared copy-on-write.
pub type SharedPixels = Arc<Vec<Color32>>;

#[derive(Debug)]
/// Tile container that is lazily filled with pixel data.
pub(crate) struct TileCell {
    data: Option<SharedPixels>,
    deep: Option<Arc<DeepTile>>,
    /// Changes with every write (see the module's notes).
    revision: u64,
    /// True if the tile contains only transparent pixels
    pub is_empty: bool,
}

impl Default for TileCell {
    fn default() -> Self {
        Self::new(None, false)
    }
}

impl TileCell {
    pub(crate) fn new(data: Option<Vec<Color32>>, is_empty: bool) -> Self {
        Self {
            data: data.map(Arc::new),
            deep: None,
            revision: next_revision(),
            is_empty,
        }
    }

    /// Its revision: a new one after every write, never the same as any
    /// other tile's.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// A tile with its 8-bit pixels and the same at full depth.
    pub(crate) fn with_deep(data: Vec<Color32>, deep: Option<DeepTile>, is_empty: bool) -> Self {
        Self::shared(Arc::new(data), deep.map(Arc::new), is_empty)
    }

    /// A tile showing pixels shared with a snapshot (copied on its first
    /// write).
    pub(crate) fn shared(data: SharedPixels, deep: Option<Arc<DeepTile>>, is_empty: bool) -> Self {
        debug_assert!(deep.as_ref().is_none_or(|d| d.len() == data.len()));
        Self {
            data: Some(data),
            deep,
            revision: next_revision(),
            is_empty,
        }
    }

    /// The 8-bit pixels.
    pub(crate) fn data(&self) -> Option<&Vec<Color32>> {
        self.data.as_deref()
    }

    /// The 8-bit pixels, to keep as they are now (shared, not copied).
    pub(crate) fn shared_data(&self) -> Option<&SharedPixels> {
        self.data.as_ref()
    }

    /// The pixels at full depth, if the tile has them.
    pub(crate) fn deep(&self) -> Option<&DeepTile> {
        self.deep.as_deref()
    }

    /// The pixels at full depth, to keep as they are now (shared).
    pub(crate) fn shared_deep(&self) -> Option<&Arc<DeepTile>> {
        self.deep.as_ref()
    }

    /// The 8-bit pixels, to write at 8 bits (the deep ones are dropped).
    pub(crate) fn data_mut(&mut self) -> Option<&mut Vec<Color32>> {
        self.revision = next_revision();
        self.deep = None;
        self.data.as_mut().map(Arc::make_mut)
    }

    /// The 8-bit pixels (transparent ones made if there were none), to
    /// write at 8 bits (the deep ones are dropped).
    pub(crate) fn data_or_insert(&mut self, len: usize) -> &mut Vec<Color32> {
        self.revision = next_revision();
        self.deep = None;
        Arc::make_mut(
            self.data
                .get_or_insert_with(|| Arc::new(vec![Color32::TRANSPARENT; len])),
        )
    }

    /// Replace the 8-bit pixels (the deep ones are dropped).
    pub(crate) fn set_data(&mut self, data: Option<Vec<Color32>>) {
        self.revision = next_revision();
        self.deep = None;
        self.data = data.map(Arc::new);
    }

    /// Replace both the 8-bit pixels and the deep ones.
    pub(crate) fn set_both(&mut self, data: Option<Vec<Color32>>, deep: Option<DeepTile>) {
        self.set_shared(data.map(Arc::new), deep.map(Arc::new));
    }

    /// Replace both with pixels shared with a snapshot.
    pub(crate) fn set_shared(&mut self, data: Option<SharedPixels>, deep: Option<Arc<DeepTile>>) {
        debug_assert!(deep.is_none() || data.is_some());
        self.revision = next_revision();
        self.data = data;
        self.deep = deep;
    }

    /// Both the 8-bit pixels (made transparent if there were none) and the
    /// deep ones at `depth` (made from the 8-bit ones if missing; `None` in
    /// an 8-bit document), for writers that keep full depth: each pixel
    /// written to the deep ones must be written, rounded, to the 8-bit ones.
    pub(crate) fn deep_parts(
        &mut self,
        depth: Depth,
        len: usize,
    ) -> (&mut Vec<Color32>, Option<&mut DeepTile>) {
        self.revision = next_revision();
        let data = Arc::make_mut(
            self.data
                .get_or_insert_with(|| Arc::new(vec![Color32::TRANSPARENT; len])),
        );
        if !depth.is_deep() {
            self.deep = None;
        } else if self.deep.as_ref().is_none_or(|d| d.depth() != depth) {
            self.deep = match &self.deep {
                Some(d) => d.convert(depth),
                None => DeepTile::widen(depth, data),
            }
            .map(Arc::new);
        }
        (data, self.deep.as_mut().map(Arc::make_mut))
    }

    /// Bytes of pixels held by this tile alone (not shared with a
    /// snapshot or another tile).
    pub(crate) fn unshared_bytes(&self) -> usize {
        (self.data.as_ref())
            .filter(|d| Arc::strong_count(d) == 1)
            .map_or(0, |d| d.len() * std::mem::size_of::<Color32>())
            + (self.deep.as_ref())
                .filter(|d| Arc::strong_count(d) == 1)
                .map_or(0, |d| d.bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_tile_is_copied_on_its_first_write_only() {
        let red = Color32::from_rgb(200, 0, 0);
        let mut cell = TileCell::new(Some(vec![red; 4]), false);
        // A snapshot shares the pixels...
        let kept = Arc::clone(cell.shared_data().unwrap());
        assert!(Arc::ptr_eq(&kept, cell.shared_data().unwrap()));
        // ...until the tile is written: then it has its own, and the
        // snapshot keeps what was there.
        cell.data_mut().unwrap()[0] = Color32::BLUE;
        assert!(!Arc::ptr_eq(&kept, cell.shared_data().unwrap()));
        assert_eq!(kept[0], red);
        assert_eq!(cell.data().unwrap()[0], Color32::BLUE);
        // Unshared, a write is in place.
        let before = Arc::as_ptr(cell.shared_data().unwrap());
        cell.data_mut().unwrap()[1] = Color32::BLUE;
        assert_eq!(before, Arc::as_ptr(cell.shared_data().unwrap()));
        // Same for full-depth writers.
        let kept = Arc::clone(cell.shared_data().unwrap());
        let (data, _) = cell.deep_parts(Depth::U8, 4);
        data[2] = Color32::GREEN;
        assert_eq!(kept[2], red);
    }

    #[test]
    fn every_write_gives_a_new_revision_no_other_tile_has() {
        let mut a = TileCell::new(Some(vec![Color32::RED; 4]), false);
        let b = TileCell::new(None, true);
        assert_ne!(a.revision(), b.revision());
        let mut seen = vec![a.revision(), b.revision()];
        let mut check = |cell: &TileCell| {
            assert!(!seen.contains(&cell.revision()));
            seen.push(cell.revision());
        };
        a.data_mut();
        check(&a);
        a.data_or_insert(4);
        check(&a);
        a.deep_parts(Depth::U8, 4);
        check(&a);
        a.set_data(None);
        check(&a);
        a.set_shared(None, None);
        check(&a);
        // Reading leaves it.
        let r = a.revision();
        let _ = (a.data(), a.shared_data(), a.deep());
        assert_eq!(a.revision(), r);
    }
}
