//! Palettes: named grids of swatches with room for gaps, and named groups
//! below the first rows (as Krita's are). The user's own are kept beside
//! the brushes; a document's are saved in its project file.

use crate::canvas::palette::{SortKey, hue_families, sort_order};
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A colour kept in a palette, and its name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Swatch {
    #[serde(with = "hex")]
    pub color: Color32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl Swatch {
    pub fn new(color: Color32) -> Self {
        Self {
            color,
            name: String::new(),
        }
    }
}

/// Some of a palette's rows, under a name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Row after row, the palette's `columns` to a row; `None` is a gap.
    /// No gaps at the end.
    pub slots: Vec<Option<Swatch>>,
}

impl Group {
    fn swatches(&self) -> impl Iterator<Item = &Swatch> {
        self.slots.iter().flatten()
    }

    fn trim(&mut self) {
        while self.slots.last().is_some_and(Option::is_none) {
            self.slots.pop();
        }
    }

    /// Gaps up to the end of the last row, so what comes next starts one.
    fn end_row(&mut self, columns: usize) {
        let len = self.slots.len().next_multiple_of(columns);
        self.slots.resize(len, None);
    }

    /// The swatches, rearranged: each list of indices into them (in the
    /// order they are now) becomes a row or more of its own.
    fn rearrange(&mut self, rows: Vec<Vec<usize>>, columns: usize) {
        let swatches: Vec<Swatch> = self.slots.drain(..).flatten().collect();
        for row in rows {
            self.slots
                .extend(row.into_iter().map(|i| Some(swatches[i].clone())));
            self.end_row(columns);
        }
        self.trim();
    }
}

/// Where a swatch is: which group, and which slot of it.
pub type Slot = (usize, usize);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Palette {
    pub name: String,
    pub columns: usize,
    /// The first is the palette's own rows (its name unused); the others
    /// are named groups below them.
    pub groups: Vec<Group>,
}

pub const DEFAULT_COLUMNS: usize = 10;
pub const MAX_COLUMNS: usize = 32;

impl Palette {
    pub fn new(name: &str, colors: &[Color32]) -> Self {
        Self {
            name: name.to_string(),
            columns: DEFAULT_COLUMNS,
            groups: vec![Group {
                name: String::new(),
                slots: colors.iter().map(|&c| Some(Swatch::new(c))).collect(),
            }],
        }
    }

    pub fn columns(&self) -> usize {
        self.columns.clamp(1, MAX_COLUMNS)
    }

    /// Fix what a file may have left wrong: at least one group, no gaps
    /// at the ends.
    pub fn tidy(&mut self) {
        if self.groups.is_empty() {
            self.groups.push(Group::default());
        }
        self.columns = self.columns();
        for g in &mut self.groups {
            g.trim();
        }
    }

    pub fn get(&self, (g, i): Slot) -> Option<&Swatch> {
        self.groups.get(g)?.slots.get(i)?.as_ref()
    }

    /// Put `swatch` in `slot` (or empty it).
    pub fn set(&mut self, (g, i): Slot, swatch: Option<Swatch>) {
        let Some(group) = self.groups.get_mut(g) else {
            return;
        };
        if i >= group.slots.len() {
            if swatch.is_none() {
                return;
            }
            group.slots.resize(i + 1, None);
        }
        group.slots[i] = swatch;
        group.trim();
    }

    /// Swap what two slots hold (moving a swatch to a gap).
    pub fn swap(&mut self, a: Slot, b: Slot) {
        if a == b {
            return;
        }
        let (sa, sb) = (self.get(a).cloned(), self.get(b).cloned());
        self.set(a, sb);
        self.set(b, sa);
    }

    pub fn colors(&self) -> impl Iterator<Item = Color32> + '_ {
        self.groups
            .iter()
            .flat_map(Group::swatches)
            .map(|s| s.color)
    }

    pub fn len(&self) -> usize {
        self.groups.iter().map(|g| g.swatches().count()).sum()
    }

    pub fn contains(&self, color: Color32) -> bool {
        self.colors().any(|c| c == color)
    }

    /// Add `color` after the first rows' last swatch, unless it's there
    /// already; whether it was added.
    pub fn push(&mut self, color: Color32) -> bool {
        if self.contains(color) {
            return false;
        }
        self.tidy();
        self.groups[0].slots.push(Some(Swatch::new(color)));
        true
    }

    /// `colors` as new rows at the end of `group`.
    pub fn add_rows(&mut self, group: usize, colors: &[Color32]) {
        self.tidy();
        let columns = self.columns();
        let Some(g) = self.groups.get_mut(group) else {
            return;
        };
        g.end_row(columns);
        g.slots.extend(colors.iter().map(|&c| Some(Swatch::new(c))));
        g.trim();
    }

    /// Each group's swatches, gaps closed, sorted by `key`.
    pub fn sort(&mut self, key: SortKey) {
        let columns = self.columns();
        for g in &mut self.groups {
            let colors: Vec<Color32> = g.swatches().map(|s| s.color).collect();
            g.rearrange(vec![sort_order(&colors, key)], columns);
        }
    }

    /// Each group's swatches in rows of a hue each (greys first), dark to
    /// light along the row.
    pub fn arrange_by_hue(&mut self) {
        let columns = self.columns();
        for g in &mut self.groups {
            let colors: Vec<Color32> = g.swatches().map(|s| s.color).collect();
            g.rearrange(hue_families(&colors), columns);
        }
    }

    /// A new group called `name` at the bottom.
    pub fn add_group(&mut self, name: &str) {
        self.groups.push(Group {
            name: name.to_string(),
            slots: Vec::new(),
        });
    }

    /// Remove group `g` (never the first rows); its swatches join the
    /// rows above it.
    pub fn remove_group(&mut self, g: usize) {
        if g == 0 || g >= self.groups.len() {
            return;
        }
        let group = self.groups.remove(g);
        let columns = self.columns();
        let into = &mut self.groups[g - 1];
        into.end_row(columns);
        into.slots.extend(group.slots);
        into.trim();
    }
}

/// Which palette: one of the user's, or one of the shown document's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Mine(usize),
    Document(usize),
}

/// Every palette, and which the colour panel shows.
pub struct Palettes {
    pub mine: Vec<Palette>,
    /// The shown document's, saved in its project file (swapped with its
    /// tab's).
    pub document: Vec<Palette>,
    active: Which,
    /// Changes to the document's palettes, which leave it unsaved.
    pub document_edits: u64,
    /// Where the user's are saved; `None` keeps them in memory.
    path: Option<PathBuf>,
    unsaved: bool,
}

impl Default for Palettes {
    fn default() -> Self {
        Self {
            mine: vec![Palette::new("Swatches", &[Color32::BLACK, Color32::WHITE])],
            document: Vec::new(),
            active: Which::Mine(0),
            document_edits: 0,
            path: None,
            unsaved: false,
        }
    }
}

/// `palettes.json`.
#[derive(Serialize, Deserialize)]
struct StoredPalettes {
    #[serde(default)]
    active: usize,
    palettes: Vec<Palette>,
}

impl Palettes {
    /// The user's palettes from `path`, saved back there when they change.
    /// The first time, the swatches of older versions (`legacy`, a list of
    /// hex colours) become the first palette.
    pub fn load(path: PathBuf, legacy: PathBuf) -> Self {
        let mut palettes = Self {
            path: Some(path.clone()),
            ..Default::default()
        };
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoredPalettes>(&bytes) {
                Ok(stored) if !stored.palettes.is_empty() => {
                    palettes.mine = stored.palettes;
                    palettes.mine.iter_mut().for_each(Palette::tidy);
                    palettes.active = Which::Mine(stored.active.min(palettes.mine.len() - 1));
                }
                Ok(_) => {}
                Err(err) => log::warn!("Ignoring {}: {err}", path.display()),
            },
            Err(_) => {
                if let Ok(bytes) = std::fs::read(legacy)
                    && let Ok(hex) = serde_json::from_slice::<Vec<String>>(&bytes)
                {
                    let colors: Vec<Color32> = hex.iter().filter_map(|h| hex::parse(h)).collect();
                    palettes.mine[0] = Palette::new("Swatches", &colors);
                    palettes.unsaved = true;
                }
            }
        }
        palettes
    }

    /// Write the user's palettes if they changed (on another thread).
    pub fn save_if_changed(&mut self) {
        if !std::mem::take(&mut self.unsaved) {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let stored = StoredPalettes {
            active: match self.active {
                Which::Mine(i) => i,
                Which::Document(_) => 0,
            },
            palettes: self.mine.clone(),
        };
        match serde_json::to_vec_pretty(&stored) {
            Ok(bytes) => crate::app::jobs::write_later(path, "palettes", move || Ok(bytes)),
            Err(err) => log::warn!("Couldn't save palettes: {err}"),
        }
    }

    pub fn get(&self, which: Which) -> Option<&Palette> {
        match which {
            Which::Mine(i) => self.mine.get(i),
            Which::Document(i) => self.document.get(i),
        }
    }

    /// Palette `which`, to change: the change is kept (saved with the
    /// user's, or leaving the document unsaved).
    pub fn edit(&mut self, which: Which) -> Option<&mut Palette> {
        match which {
            Which::Mine(i) => {
                self.unsaved = true;
                self.mine.get_mut(i)
            }
            Which::Document(i) => {
                self.document_edits += 1;
                self.document.get_mut(i)
            }
        }
    }

    pub fn active_which(&self) -> Which {
        if self.get(self.active).is_some() {
            self.active
        } else {
            Which::Mine(0)
        }
    }

    pub fn active(&self) -> &Palette {
        self.get(self.active_which())
            .or_else(|| self.mine.first())
            .expect("there is always one of the user's palettes")
    }

    /// The palette shown, to change.
    pub fn edit_active(&mut self) -> &mut Palette {
        let which = self.active_which();
        self.edit(which).expect("the active palette exists")
    }

    pub fn set_active(&mut self, which: Which) {
        if self.get(which).is_some() && which != self.active {
            self.active = which;
            if matches!(which, Which::Mine(_)) {
                self.unsaved = true;
            }
        }
    }

    /// Every palette, the user's first.
    pub fn all(&self) -> impl Iterator<Item = (Which, &Palette)> {
        let mine = self
            .mine
            .iter()
            .enumerate()
            .map(|(i, p)| (Which::Mine(i), p));
        let doc = (self.document.iter().enumerate()).map(|(i, p)| (Which::Document(i), p));
        mine.chain(doc)
    }

    /// Add `palette` (to the document's, or the user's) and show it.
    pub fn add(&mut self, mut palette: Palette, in_document: bool) -> Which {
        palette.tidy();
        let which = if in_document {
            self.document.push(palette);
            self.document_edits += 1;
            Which::Document(self.document.len() - 1)
        } else {
            self.mine.push(palette);
            self.unsaved = true;
            Which::Mine(self.mine.len() - 1)
        };
        self.set_active(which);
        which
    }

    /// Remove palette `which`. The user always keeps one.
    pub fn remove(&mut self, which: Which) {
        let active = self.active_which();
        match which {
            Which::Mine(i) if i < self.mine.len() => {
                self.mine.remove(i);
                if self.mine.is_empty() {
                    self.mine.push(Palette::new("Swatches", &[]));
                }
                self.unsaved = true;
            }
            Which::Document(i) if i < self.document.len() => {
                self.document.remove(i);
                self.document_edits += 1;
            }
            _ => return,
        }
        // The one shown keeps showing, unless it's the one gone.
        self.active = match (active, which) {
            (a, w) if a == w => Which::Mine(0),
            (Which::Mine(a), Which::Mine(w)) if a > w => Which::Mine(a - 1),
            (Which::Document(a), Which::Document(w)) if a > w => Which::Document(a - 1),
            (a, _) => a,
        };
    }

    /// Move palette `which` into the document's (`true`) or the user's.
    pub fn move_to(&mut self, which: Which, document: bool) {
        let Some(palette) = self.get(which).cloned() else {
            return;
        };
        if matches!(which, Which::Document(_)) == document {
            return;
        }
        let shown = self.active_which() == which;
        self.remove(which);
        let kept = self.active;
        let new = self.add(palette, document);
        self.active = if shown { new } else { kept };
    }

    /// A new document: it has no palettes of its own yet.
    pub fn forget_document(&mut self) {
        self.document.clear();
        self.document_edits = 0;
    }

    /// Swap the shown document's palettes with another tab's.
    pub fn swap_document(&mut self, palettes: &mut Vec<Palette>, edits: &mut u64) {
        std::mem::swap(&mut self.document, palettes);
        std::mem::swap(&mut self.document_edits, edits);
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::default()
    }
}

/// Colours as `#RRGGBBAA` (unmultiplied), as `swatches.json` kept them.
pub(crate) mod hex {
    use eframe::egui::Color32;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn format(c: Color32) -> String {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        if a == 255 {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
        }
    }

    /// `#RRGGBB` or `#RRGGBBAA`.
    pub fn parse(hex: &str) -> Option<Color32> {
        let h = hex.strip_prefix('#')?;
        if h.len() != 6 && h.len() != 8 {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        let a = if h.len() == 8 { byte(6)? } else { 255 };
        Some(Color32::from_rgba_unmultiplied(
            byte(0)?,
            byte(2)?,
            byte(4)?,
            a,
        ))
    }

    pub fn serialize<S: Serializer>(c: &Color32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format(*c))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color32, D::Error> {
        let s = String::deserialize(d)?;
        parse(&s).ok_or_else(|| serde::de::Error::custom(format!("not a colour: {s}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color32 = Color32::from_rgb(220, 30, 30);
    const DARK_RED: Color32 = Color32::from_rgb(90, 10, 10);
    const BLUE: Color32 = Color32::from_rgb(30, 40, 220);
    const GREY: Color32 = Color32::from_rgb(128, 128, 128);

    #[test]
    fn colours_read_back_as_written() {
        assert_eq!(hex::parse("#FF8000"), Some(Color32::from_rgb(255, 128, 0)));
        assert_eq!(
            hex::parse("#10203080"),
            Some(Color32::from_rgba_unmultiplied(16, 32, 48, 128))
        );
        assert_eq!(hex::parse("FF8000"), None);
        assert_eq!(hex::parse("#GG0000"), None);
        let half = Color32::from_rgba_unmultiplied(16, 32, 48, 128);
        assert_eq!(hex::parse(&hex::format(half)), Some(half));
    }

    #[test]
    fn swatches_move_into_gaps_and_gaps_stay() {
        let mut p = Palette::new("p", &[RED, BLUE]);
        p.swap((0, 0), (0, 5));
        assert_eq!(p.get((0, 0)), None);
        assert_eq!(p.get((0, 5)).map(|s| s.color), Some(RED));
        assert_eq!(p.groups[0].slots.len(), 6);
        // Emptying the last slot drops the gaps before it too.
        p.set((0, 5), None);
        assert_eq!(p.groups[0].slots.len(), 2);
        assert!(!p.push(BLUE), "already there");
        assert!(p.push(GREY));
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn rows_start_on_a_new_row_and_groups_merge_up() {
        let mut p = Palette::new("p", &[RED]);
        p.columns = 4;
        p.add_rows(0, &[BLUE, GREY]);
        assert_eq!(p.groups[0].slots.len(), 6);
        assert_eq!(p.get((0, 4)).map(|s| s.color), Some(BLUE));
        p.add_group("Skin");
        p.add_rows(1, &[DARK_RED]);
        p.remove_group(1);
        assert_eq!(p.groups.len(), 1);
        assert_eq!(p.get((0, 8)).map(|s| s.color), Some(DARK_RED));
    }

    #[test]
    fn sorting_closes_gaps_and_arranging_gives_each_hue_a_row() {
        let mut p = Palette::new("p", &[RED, GREY, BLUE, DARK_RED]);
        p.columns = 4;
        p.set((0, 7), Some(Swatch::new(Color32::WHITE)));
        p.sort(SortKey::Lightness);
        let colors: Vec<_> = p.colors().collect();
        assert_eq!(colors.len(), 5);
        assert_eq!(colors[0], DARK_RED);
        assert_eq!(*colors.last().unwrap(), Color32::WHITE);
        assert_eq!(p.groups[0].slots.len(), 5);

        p.arrange_by_hue();
        // Greys, reds (dark to light), blue: each its own row.
        let row = |r: usize| -> Vec<Color32> {
            (0..4)
                .filter_map(|c| p.get((0, r * 4 + c)).map(|s| s.color))
                .collect()
        };
        assert_eq!(row(0), vec![GREY, Color32::WHITE]);
        assert_eq!(row(1), vec![DARK_RED, RED]);
        assert_eq!(row(2), vec![BLUE]);
    }

    #[test]
    fn removing_keeps_one_and_the_shown_one_shown() {
        let mut ps = Palettes::in_memory();
        ps.add(Palette::new("a", &[RED]), false);
        let doc = ps.add(Palette::new("d", &[BLUE]), true);
        assert_eq!(ps.active().name, "d");
        ps.set_active(Which::Mine(1));
        ps.remove(Which::Mine(0));
        assert_eq!(ps.active().name, "a");
        ps.move_to(doc, false);
        assert!(ps.document.is_empty());
        assert_eq!(ps.mine.len(), 2);
        assert_eq!(ps.active().name, "a");
        ps.remove(Which::Mine(0));
        ps.remove(Which::Mine(0));
        assert_eq!(ps.mine.len(), 1, "always one left");
        // A document's palette that went with its tab isn't shown.
        let doc = ps.add(Palette::new("d", &[]), true);
        ps.forget_document();
        assert_ne!(ps.active_which(), doc);
    }
}
