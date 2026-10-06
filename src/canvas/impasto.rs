//! Impasto: how thick the paint is, as a height per pixel beside a layer's
//! colour ([`HeightMap`]), and the light that shows it ([`ImpastoLight`]):
//! the layer's own pixels shaded by the slope of the paint, worked out as
//! the layer is composited (so the screen, merging and export show it).

use eframe::egui::Color32;
use rustc_hash::FxHashMap;
use std::sync::Mutex;

/// Height tiles as a step changes them (`None`: flat).
pub type HeightTiles = Vec<((i32, i32), Option<Vec<u16>>)>;
/// A whole map's tiles.
pub type MapTiles = Vec<((i32, i32), Vec<u16>)>;

/// The tallest paint, canvas pixels (a height of `u16::MAX`).
pub const MAX_HEIGHT: f32 = 24.0;

/// A layer's paint heights, tile by tile (missing tiles are flat), as the
/// layer's own tiles are laid out.
#[derive(Debug, Default)]
pub struct HeightMap {
    tiles: Mutex<FxHashMap<(i32, i32), Vec<u16>>>,
}

impl Clone for HeightMap {
    fn clone(&self) -> Self {
        Self {
            tiles: Mutex::new(self.lock().clone()),
        }
    }
}

impl PartialEq for HeightMap {
    fn eq(&self, other: &Self) -> bool {
        *self.lock() == *other.lock()
    }
}

impl HeightMap {
    fn lock(&self) -> std::sync::MutexGuard<'_, FxHashMap<(i32, i32), Vec<u16>>> {
        self.tiles.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tile `key`'s heights, if it has any.
    pub fn tile(&self, key: (i32, i32)) -> Option<Vec<u16>> {
        self.lock().get(&key).cloned()
    }

    /// Put tile `key`'s heights (`None`: flat), returning what was there.
    pub fn set_tile(&self, key: (i32, i32), heights: Option<Vec<u16>>) -> Option<Vec<u16>> {
        let mut tiles = self.lock();
        match heights {
            Some(h) if h.iter().any(|&v| v > 0) => tiles.insert(key, h),
            _ => tiles.remove(&key),
        }
    }

    /// Change tile `key`'s heights in place (made flat first if missing).
    pub fn edit_tile(&self, key: (i32, i32), side: usize, edit: impl FnOnce(&mut [u16])) {
        let mut tiles = self.lock();
        let tile = tiles.entry(key).or_insert_with(|| vec![0; side * side]);
        edit(tile);
    }

    /// Every tile, sorted (for saving).
    pub fn tiles(&self) -> Vec<((i32, i32), Vec<u16>)> {
        let mut all: Vec<_> = self.lock().iter().map(|(&k, v)| (k, v.clone())).collect();
        all.sort_by_key(|(k, _)| (k.1, k.0));
        all
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Memory held, bytes.
    pub fn bytes(&self) -> usize {
        self.lock().values().map(|t| t.len() * 2).sum()
    }

    /// The heights around tile `(tx, ty)` and a pixel past each edge, as a
    /// `(side + 2)²` patch (row-major), for slopes at the tile's edges.
    fn patch(&self, tx: i32, ty: i32, side: usize) -> Option<Vec<u16>> {
        let tiles = self.lock();
        let s = side as i32;
        let mut any = false;
        let mut out = vec![0u16; (side + 2) * (side + 2)];
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let Some(t) = tiles.get(&(tx + dx, ty + dy)) else {
                    continue;
                };
                any = true;
                // This tile's part of the patch (only its border pixel
                // for a neighbour).
                let (x0, x1) = match dx {
                    -1 => (s - 1, s),
                    0 => (0, s),
                    _ => (0, 1),
                };
                let (y0, y1) = match dy {
                    -1 => (s - 1, s),
                    0 => (0, s),
                    _ => (0, 1),
                };
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (px, py) = (dx * s + x + 1, dy * s + y + 1);
                        out[py as usize * (side + 2) + px as usize] = t[(y * s + x) as usize];
                    }
                }
            }
        }
        any.then_some(out)
    }
}

/// Note tile `key` of layer `layer`'s heights as they were (`before`) in
/// `undo` (once per tile: a stroke's first touch).
pub fn record_undo(
    undo: &mut crate::canvas::history::UndoAction,
    layer: crate::canvas::storage::LayerId,
    key: (i32, i32),
    before: Option<Vec<u16>>,
) {
    use crate::canvas::history::LayerHistoryOp;
    match &mut undo.layer_action {
        Some(LayerHistoryOp::Height {
            layer: l, tiles, ..
        }) if *l == layer => {
            if !tiles.iter().any(|(k, _)| *k == key) {
                tiles.push((key, before));
            }
        }
        other => {
            let inner = other.take().map(Box::new);
            *other = Some(LayerHistoryOp::Height {
                layer,
                tiles: vec![(key, before)],
                map: None,
                inner,
            });
        }
    }
}

/// Heights as pixels (big end in red, small in green, opaque), so the
/// whole-document image operations move them with the paint.
pub fn to_pixels(heights: &[u16]) -> Vec<Color32> {
    heights
        .iter()
        .map(|&h| Color32::from_rgba_premultiplied((h >> 8) as u8, h as u8, 0, 255))
        .collect()
}

pub fn from_pixels(pixels: &[Color32]) -> Vec<u16> {
    pixels
        .iter()
        .map(|c| ((c.r() as u16) << 8) | c.g() as u16)
        .collect()
}

/// The light on a layer's paint.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ImpastoLight {
    /// Where it comes from, degrees counter-clockwise from the right.
    pub angle: f32,
    /// How high it is, degrees above the canvas.
    pub elevation: f32,
    /// How strongly slopes are lit and shaded (0..2).
    pub strength: f32,
    /// The shine on slopes facing it (0..1).
    pub gloss: f32,
}

impl Default for ImpastoLight {
    fn default() -> Self {
        Self {
            angle: 135.0,
            elevation: 45.0,
            strength: 1.0,
            gloss: 0.3,
        }
    }
}

impl ImpastoLight {
    /// Tile `(tx, ty)`'s `pixels` (premultiplied) lit by `heights`: flat
    /// paint stays exactly as it is.
    pub fn shade(
        &self,
        pixels: &mut [Color32],
        heights: &HeightMap,
        tx: i32,
        ty: i32,
        side: usize,
    ) {
        let Some(h) = heights.patch(tx, ty, side) else {
            return;
        };
        let (sa, ca) = self.angle.to_radians().sin_cos();
        let (se, ce) = self.elevation.clamp(1.0, 90.0).to_radians().sin_cos();
        // Towards the light (y down on screen, so up is -y).
        let light = [ca * ce, -sa * ce, se];
        // Half way to the viewer, straight above.
        let half = normalize([light[0], light[1], light[2] + 1.0]);
        let shine = |n: [f32; 3]| dot(n, half).max(0.0).powi(24);
        let flat_shine = shine([0.0, 0.0, 1.0]);
        let strength = self.strength.clamp(0.0, 2.0);
        let gloss = self.gloss.clamp(0.0, 1.0);
        let k = MAX_HEIGHT / u16::MAX as f32;
        let w = side + 2;
        for y in 0..side {
            for x in 0..side {
                let at = |dx: i32, dy: i32| {
                    h[(y as i32 + 1 + dy) as usize * w + (x as i32 + 1 + dx) as usize] as f32 * k
                };
                let (gx, gy) = ((at(1, 0) - at(-1, 0)) * 0.5, (at(0, 1) - at(0, -1)) * 0.5);
                if gx == 0.0 && gy == 0.0 {
                    continue;
                }
                let px = &mut pixels[y * side + x];
                if px.a() == 0 {
                    continue;
                }
                let n = normalize([-gx, -gy, 1.0]);
                let lit = 1.0 + strength * (dot(n, light).max(0.0) / se - 1.0);
                let spec = gloss * (shine(n) - flat_shine).max(0.0) * px.a() as f32;
                let [r, g, b, a] = px.to_array();
                let f = |c: u8| {
                    (c as f32 * lit.max(0.0) + spec)
                        .round()
                        .clamp(0.0, a as f32) as u8
                };
                *px = Color32::from_rgba_premultiplied(f(r), f(g), f(b), a);
            }
        }
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = dot(v, v).sqrt().max(1e-9);
    v.map(|c| c / l)
}

/// How a brush lays paint thickness down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImpastoMode {
    /// Builds up on what's there.
    #[default]
    Add,
    /// Up to this thickness, never lowering what's thicker.
    Max,
    /// Smooths it back down to the canvas.
    Flatten,
}

/// A brush's paint thickness (see [`HeightMap`]).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Impasto {
    /// How thick a full dab is, a share of the tallest paint.
    pub depth: f32,
    pub mode: ImpastoMode,
}

impl Default for Impasto {
    fn default() -> Self {
        Self {
            depth: 0.3,
            mode: ImpastoMode::Add,
        }
    }
}

impl Impasto {
    /// The height after paint of `coverage` (0..1) over `before`; an
    /// eraser (`erase`) takes the paint's thickness away with it.
    pub fn height(&self, before: u16, coverage: f32, erase: bool) -> u16 {
        let c = coverage.clamp(0.0, 1.0);
        let h = before as f32;
        let laid = self.depth.clamp(0.0, 1.0) * u16::MAX as f32 * c;
        let out = if erase {
            h * (1.0 - c)
        } else {
            match self.mode {
                ImpastoMode::Add => h + laid,
                ImpastoMode::Max => h.max(laid),
                ImpastoMode::Flatten => h * (1.0 - c),
            }
        };
        out.round().clamp(0.0, u16::MAX as f32) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: usize = 16;

    /// A tile with a ridge down column 8 (heights `h` there).
    fn ridge(h: u16) -> HeightMap {
        let map = HeightMap::default();
        let mut t = vec![0u16; S * S];
        for y in 0..S {
            t[y * S + 8] = h;
        }
        map.set_tile((0, 0), Some(t));
        map
    }

    #[test]
    fn flat_paint_is_left_exactly_as_it_is() {
        let grey = Color32::from_rgb(120, 130, 140);
        let mut px = vec![grey; S * S];
        ImpastoLight::default().shade(&mut px, &HeightMap::default(), 0, 0, S);
        assert!(px.iter().all(|&c| c == grey), "no heights at all");
        let map = HeightMap::default();
        map.set_tile((0, 0), Some(vec![500; S * S]));
        ImpastoLight::default().shade(&mut px, &map, 0, 0, S);
        // (Its edges slope down to the flat tiles around it.)
        assert!(px[5 * S + 5] == grey, "a level plateau");
    }

    #[test]
    fn a_ridge_lit_from_the_left_is_brighter_on_its_left() {
        let grey = Color32::from_rgb(120, 120, 120);
        let mut px = vec![grey; S * S];
        let light = ImpastoLight {
            angle: 180.0,
            gloss: 0.0,
            ..Default::default()
        };
        light.shade(&mut px, &ridge(u16::MAX / 2), 0, 0, S);
        let (left, right) = (px[5 * S + 7], px[5 * S + 9]);
        assert!(left.r() > 120 && right.r() < 120, "{left:?} {right:?}");
        assert_eq!(px[5 * S + 2], grey, "away from it, flat");
        // Transparent pixels stay transparent.
        let mut clear = vec![Color32::TRANSPARENT; S * S];
        light.shade(&mut clear, &ridge(u16::MAX), 0, 0, S);
        assert!(clear.iter().all(|&c| c == Color32::TRANSPARENT));
    }

    #[test]
    fn heights_survive_the_trip_through_pixels_and_maps_drop_flat_tiles() {
        let h = vec![0, 1, 255, 256, 40_000, u16::MAX];
        assert_eq!(from_pixels(&to_pixels(&h)), h);
        let map = HeightMap::default();
        map.set_tile((1, 1), Some(vec![0; 4]));
        assert!(map.is_empty(), "flat is no tile");
        map.set_tile((1, 1), Some(vec![0, 3, 0, 0]));
        assert_eq!(map.tile((1, 1)), Some(vec![0, 3, 0, 0]));
        assert_eq!(map.bytes(), 8);
    }

    #[test]
    fn brushes_build_up_cap_flatten_and_erase_heights() {
        let add = Impasto::default();
        let once = add.height(0, 1.0, false);
        assert!(add.height(once, 1.0, false) > once, "builds up");
        assert_eq!(add.height(1000, 0.0, false), 1000, "nothing laid");
        let max = Impasto {
            mode: ImpastoMode::Max,
            ..add
        };
        assert_eq!(max.height(once, 1.0, false), once, "never past its depth");
        let flat = Impasto {
            mode: ImpastoMode::Flatten,
            ..add
        };
        assert_eq!(flat.height(1000, 1.0, false), 0);
        assert_eq!(add.height(1000, 0.5, true), 500, "an eraser takes it down");
    }
}
