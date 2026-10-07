//! A tile's storage: its 8-bit pixels and, in a deeper document, the same
//! pixels at full depth (see [`super::deep`]). The fields are private so
//! that every write to the 8-bit pixels goes through here, which drops the
//! deep pixels it would leave stale; full-depth writers use
//! [`TileCell::deep_parts`] and keep both.

use super::deep::{DeepTile, Depth};
use eframe::egui::Color32;

#[derive(Debug, Default)]
/// Tile container that is lazily filled with pixel data.
pub(crate) struct TileCell {
    data: Option<Vec<Color32>>,
    deep: Option<DeepTile>,
    /// True if the tile contains only transparent pixels
    pub is_empty: bool,
}

impl TileCell {
    pub(crate) fn new(data: Option<Vec<Color32>>, is_empty: bool) -> Self {
        Self {
            data,
            deep: None,
            is_empty,
        }
    }

    /// A tile with its 8-bit pixels and the same at full depth.
    pub(crate) fn with_deep(data: Vec<Color32>, deep: Option<DeepTile>, is_empty: bool) -> Self {
        debug_assert!(deep.as_ref().is_none_or(|d| d.len() == data.len()));
        Self {
            data: Some(data),
            deep,
            is_empty,
        }
    }

    /// The 8-bit pixels.
    pub(crate) fn data(&self) -> Option<&Vec<Color32>> {
        self.data.as_ref()
    }

    /// The pixels at full depth, if the tile has them.
    pub(crate) fn deep(&self) -> Option<&DeepTile> {
        self.deep.as_ref()
    }

    /// The 8-bit pixels, to write at 8 bits (the deep ones are dropped).
    pub(crate) fn data_mut(&mut self) -> Option<&mut Vec<Color32>> {
        self.deep = None;
        self.data.as_mut()
    }

    /// The 8-bit pixels (transparent ones made if there were none), to
    /// write at 8 bits (the deep ones are dropped).
    pub(crate) fn data_or_insert(&mut self, len: usize) -> &mut Vec<Color32> {
        self.deep = None;
        self.data
            .get_or_insert_with(|| vec![Color32::TRANSPARENT; len])
    }

    /// Replace the 8-bit pixels (the deep ones are dropped).
    pub(crate) fn set_data(&mut self, data: Option<Vec<Color32>>) {
        self.deep = None;
        self.data = data;
    }

    /// Replace both the 8-bit pixels and the deep ones.
    pub(crate) fn set_both(&mut self, data: Option<Vec<Color32>>, deep: Option<DeepTile>) {
        debug_assert!(deep.is_none() || data.is_some());
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
        let data = self
            .data
            .get_or_insert_with(|| vec![Color32::TRANSPARENT; len]);
        if !depth.is_deep() {
            self.deep = None;
        } else if self.deep.as_ref().is_none_or(|d| d.depth() != depth) {
            self.deep = match &self.deep {
                Some(d) => d.convert(depth),
                None => DeepTile::widen(depth, data),
            };
        }
        (data, self.deep.as_mut())
    }

    /// Bytes of pixels held.
    pub(crate) fn bytes(&self) -> usize {
        self.data
            .as_ref()
            .map_or(0, |d| d.len() * std::mem::size_of::<Color32>())
            + self.deep.as_ref().map_or(0, DeepTile::bytes)
    }
}
