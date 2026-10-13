//! Layer styles: fill layers (a colour or gradient instead of paint) and a
//! border around a layer's paint, as Clip Studio's border effect. Nothing is stored as pixels: the compositor asks
//! [`LayerStyle`] for each tile as it would read a painted one.

use crate::canvas::blend_modes::{BlendSpace, pixel_noise};
use crate::canvas::filters::GradientMap;
use crate::canvas::gradient::{Gradient, GradientRepeat, GradientShape, Ramp};
use eframe::egui::{Color32, Vec2};

/// Widest border, in pixels: its spill stays within the next tile.
pub const MAX_BORDER: f32 = 48.0;

/// What's generated for a layer rather than painted.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LayerStyle {
    /// A fill layer: this colour or gradient everywhere (its mask and
    /// clipping say where it shows).
    #[serde(default)]
    pub fill: Option<LayerFill>,
    /// An outline around the layer's paint.
    #[serde(default)]
    pub border: Option<Border>,
    /// The light on the paint's thickness (the layer's impasto heights).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impasto: Option<crate::canvas::impasto::ImpastoLight>,
    /// The layer's heights are a lightness map instead (Krita's colour
    /// smudge with a lightness tip): grey and its coverage, lightening and
    /// darkening the paint (see [`crate::canvas::impasto::modulate`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lightness_map: bool,
}

impl LayerStyle {
    pub fn is_plain(&self) -> bool {
        self.fill.is_none()
            && self.border.is_none()
            && self.impasto.is_none()
            && !self.lightness_map
    }

    /// How far (pixels) past its paint the layer shows: the border's width.
    pub fn reach(&self) -> i32 {
        let border = match (&self.fill, self.border) {
            (None, Some(b)) => b.width.clamp(0.0, MAX_BORDER).ceil() as i32 + 1,
            _ => 0,
        };
        // Impasto's slopes read a pixel of the next tile.
        border.max(self.impasto.is_some() as i32)
    }
}

/// A fill layer's content.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LayerFill {
    /// One sRGB colour.
    Colour([u8; 3]),
    /// A gradient between two points (canvas pixels).
    Gradient {
        colours: GradientMap,
        shape: GradientShape,
        start: [f32; 2],
        end: [f32; 2],
        #[serde(default)]
        repeat: GradientRepeat,
        #[serde(default)]
        reverse: bool,
    },
}

thread_local! {
    /// The last gradient each thread turned into a ramp: a fill's tiles are
    /// made one by one, on whichever thread composites them.
    // A single entry: two gradient fills composited in turn on one thread
    // rebuild each other's ramp (4096 colours) per tile; key a small map by
    // layer if documents with several gradient fills feel slow.
    static RAMP: std::cell::RefCell<Option<(GradientMap, BlendSpace, std::sync::Arc<Ramp>)>> =
        const { std::cell::RefCell::new(None) };
}

/// `stops` as a ramp mixed in `space`, from this thread's cache if it
/// already made it.
fn ramp(stops: &GradientMap, space: BlendSpace) -> std::sync::Arc<Ramp> {
    RAMP.with(|cell| {
        let mut cell = cell.borrow_mut();
        if let Some((s, sp, ramp)) = cell.as_ref()
            && s == stops
            && *sp == space
        {
            return std::sync::Arc::clone(ramp);
        }
        let ramp = std::sync::Arc::new(Ramp::new(stops, space, 1.0));
        *cell = Some((stops.clone(), space, std::sync::Arc::clone(&ramp)));
        ramp
    })
}

impl LayerFill {
    pub fn name(&self) -> &'static str {
        match self {
            LayerFill::Colour(_) => "Colour Fill",
            LayerFill::Gradient { .. } => "Gradient Fill",
        }
    }

    /// The colour at canvas pixel `(x, y)` (a thumbnail's sample).
    pub fn pixel(&self, x: i32, y: i32, space: BlendSpace) -> Color32 {
        match self {
            LayerFill::Colour([r, g, b]) => Color32::from_rgb(*r, *g, *b),
            LayerFill::Gradient { colours, .. } => {
                let t = self.gradient().map_or(0.0, |g| {
                    g.position(Vec2::new(x as f32 + 0.5, y as f32 + 0.5))
                });
                ramp(colours, space).pixel(t, None)
            }
        }
    }

    pub fn gradient(&self) -> Option<Gradient> {
        match *self {
            LayerFill::Gradient {
                shape,
                start,
                end,
                repeat,
                reverse,
                ..
            } => Some(Gradient {
                shape,
                repeat,
                start: Vec2::from(start),
                end: Vec2::from(end),
                reverse,
            }),
            LayerFill::Colour(_) => None,
        }
    }

    /// Tile `(tx, ty)` of a `tile_size` grid, premultiplied: a gradient
    /// mixed in `space` and dithered.
    pub fn tile(&self, tx: i32, ty: i32, tile_size: usize, space: BlendSpace) -> Vec<Color32> {
        match self {
            LayerFill::Colour([r, g, b]) => {
                vec![Color32::from_rgb(*r, *g, *b); tile_size * tile_size]
            }
            LayerFill::Gradient { colours, .. } => {
                let (gradient, ramp) = (
                    self.gradient().expect("a gradient fill"),
                    ramp(colours, space),
                );
                let mut out = Vec::with_capacity(tile_size * tile_size);
                let mut row = vec![0.0; tile_size];
                let (x0, y0) = (tx * tile_size as i32, ty * tile_size as i32);
                for y in y0..y0 + tile_size as i32 {
                    gradient.row_positions(x0, y, &mut row);
                    out.extend(row.iter().enumerate().map(|(i, &t)| {
                        let noise = pixel_noise((x0 + i as i32) as u32, y as u32);
                        ramp.pixel(t, Some(noise))
                    }));
                }
                out
            }
        }
    }

    /// Tile `(tx, ty)` at full precision, premultiplied in `space`: for a
    /// deeper document, where a gradient needs no dither. `None` for a
    /// plain colour, which 8 bits hold exactly.
    pub fn tile_precise(
        &self,
        tx: i32,
        ty: i32,
        tile_size: usize,
        space: BlendSpace,
    ) -> Option<Vec<[f32; 4]>> {
        let LayerFill::Gradient { colours, .. } = self else {
            return None;
        };
        let (gradient, ramp) = (self.gradient()?, ramp(colours, space));
        let mut out = Vec::with_capacity(tile_size * tile_size);
        let mut row = vec![0.0; tile_size];
        let (x0, y0) = (tx * tile_size as i32, ty * tile_size as i32);
        for y in y0..y0 + tile_size as i32 {
            gradient.row_positions(x0, y, &mut row);
            out.extend(row.iter().map(|&t| ramp.premultiplied_in(t, space)));
        }
        Some(out)
    }
}

/// An outline around a layer's paint.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Border {
    /// Pixels, 1..=[`MAX_BORDER`].
    pub width: f32,
    pub colour: [u8; 3],
    pub opacity: f32,
}

impl Default for Border {
    fn default() -> Self {
        Self {
            width: 4.0,
            colour: [255, 255, 255],
            opacity: 1.0,
        }
    }
}

impl Border {
    /// The layer's tile with the border under it. `alpha` is the layer's
    /// alpha over the `side`×`side` square around the tile (`reach` pixels
    /// each way), `pixels` the tile itself (premultiplied, `tile_size`
    /// wide, or `None` where it has no paint).
    pub fn apply(
        &self,
        alpha: &[u8],
        side: usize,
        reach: usize,
        pixels: Option<&[Color32]>,
        tile_size: usize,
    ) -> Vec<Color32> {
        let dist = distance_to_paint(alpha, side);
        let r = self.width.clamp(0.0, MAX_BORDER);
        let opacity = self.opacity.clamp(0.0, 1.0);
        let [cr, cg, cb] = self.colour;
        let mut out = Vec::with_capacity(tile_size * tile_size);
        for y in 0..tile_size {
            let row = (y + reach) * side + reach;
            for x in 0..tile_size {
                let d = dist[row + x].sqrt();
                // `d` runs centre to centre: the pixel `r` out from the
                // paint is the border's last whole one; the next fades.
                let coverage = (r + 1.0 - d).clamp(0.0, 1.0) * opacity;
                let under =
                    Color32::from_rgba_unmultiplied(cr, cg, cb, (coverage * 255.0).round() as u8);
                let paint = pixels.map_or(Color32::TRANSPARENT, |p| p[y * tile_size + x]);
                out.push(crate::canvas::storage::gamma_over(paint, under));
            }
        }
        out
    }
}

/// Squared distance from each pixel of the `side`×`side` `alpha` to the
/// nearest painted one (alpha at least half), by the exact Euclidean
/// distance transform (Felzenszwalb & Huttenlocher), in linear time.
fn distance_to_paint(alpha: &[u8], side: usize) -> Vec<f32> {
    const FAR: f32 = 1e10;
    let mut d: Vec<f32> = alpha
        .iter()
        .map(|&a| if a >= 128 { 0.0 } else { FAR })
        .collect();
    let mut line = vec![0.0; side];
    let mut out = vec![0.0; side];
    let (mut v, mut z) = (vec![0usize; side], vec![0.0f32; side + 1]);
    // Columns, then rows.
    for x in 0..side {
        for y in 0..side {
            line[y] = d[y * side + x];
        }
        distance_1d(&line, &mut out, &mut v, &mut z);
        for y in 0..side {
            d[y * side + x] = out[y];
        }
    }
    for y in 0..side {
        line.copy_from_slice(&d[y * side..(y + 1) * side]);
        distance_1d(&line, &mut out, &mut v, &mut z);
        d[y * side..(y + 1) * side].copy_from_slice(&out);
    }
    d
}

/// One line of the distance transform: `out[q]` = min over p of
/// `(q - p)² + f[p]`, by the lower envelope of parabolas.
fn distance_1d(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q - p) as f32);
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0: the new parabola replaces the first.
                v[0] = q;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f32 - p as f32;
        *o = dq * dq + f[p];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_match_brute_force() {
        let side = 23;
        let alpha: Vec<u8> = (0..side * side)
            .map(|i| if (i * 7919) % 37 == 0 { 255 } else { 0 })
            .collect();
        let fast = distance_to_paint(&alpha, side);
        for y in 0..side {
            for x in 0..side {
                let mut best = f32::MAX;
                for py in 0..side {
                    for px in 0..side {
                        if alpha[py * side + px] >= 128 {
                            let (dx, dy) = (x as f32 - px as f32, y as f32 - py as f32);
                            best = best.min(dx * dx + dy * dy);
                        }
                    }
                }
                assert_eq!(fast[y * side + x], best, "at {x},{y}");
            }
        }
    }

    #[test]
    fn a_border_rings_the_paint_at_its_width() {
        // A dot in the middle of a 16 px tile with 8 px of margin.
        let (ts, reach) = (16usize, 8usize);
        let side = ts + 2 * reach;
        let mut alpha = vec![0u8; side * side];
        let c = reach + 8;
        alpha[c * side + c] = 255;
        let mut pixels = vec![Color32::TRANSPARENT; ts * ts];
        pixels[8 * ts + 8] = Color32::BLACK;
        let border = Border {
            width: 3.0,
            colour: [255, 0, 0],
            opacity: 1.0,
        };
        let out = border.apply(&alpha, side, reach, Some(&pixels), ts);
        assert_eq!(out[8 * ts + 8], Color32::BLACK, "the paint stays on top");
        assert_eq!(out[8 * ts + 11], Color32::RED, "3 px out: border");
        assert_eq!(out[8 * ts + 13], Color32::TRANSPARENT, "5 px out: nothing");
        // Just past the width, it fades.
        let edge = out[(8 + 3) * ts + 8 + 1].a();
        assert!(edge > 0 && edge < 255, "{edge}");
    }

    #[test]
    fn fills_cover_their_tile() {
        let solid = LayerFill::Colour([10, 20, 30]).tile(3, 1, 8, BlendSpace::Gamma);
        assert!(solid.iter().all(|&c| c == Color32::from_rgb(10, 20, 30)));
        let gradient = LayerFill::Gradient {
            colours: GradientMap::default(),
            shape: GradientShape::Linear,
            start: [0.0, 0.0],
            end: [16.0, 0.0],
            repeat: GradientRepeat::None,
            reverse: false,
        };
        // Left tile dark to mid, right tile mid to light.
        let left = gradient.tile(0, 0, 8, BlendSpace::Gamma);
        let right = gradient.tile(1, 0, 8, BlendSpace::Gamma);
        assert!(left[0].r() < 20 && right[7].r() > 235);
        assert!(left[7].r() < right[0].r());
        assert!(
            left[0].r().abs_diff(left[8 * 7].r()) <= 1,
            "same down the column, but for dither"
        );
        // In a deeper document: exact, and in step with the 8-bit tile.
        let precise = gradient.tile_precise(1, 0, 8, BlendSpace::Gamma).unwrap();
        assert!((precise[3][0] * 255.0 - right[3].r() as f32).abs() <= 1.0);
        assert!(
            LayerFill::Colour([1, 2, 3])
                .tile_precise(0, 0, 8, BlendSpace::Gamma)
                .is_none()
        );
    }

    #[test]
    fn a_fill_saved_before_repeat_and_reverse_still_reads() {
        let old = r#"{"Gradient":{"colours":{"len":2,"stops":[[0.0,[0,0,0]],[1.0,[255,255,255]],[0.0,[0,0,0]],[0.0,[0,0,0]],[0.0,[0,0,0]],[0.0,[0,0,0]],[0.0,[0,0,0]],[0.0,[0,0,0]]]},"shape":"Radial","start":[1.0,2.0],"end":[3.0,4.0]}}"#;
        let fill: LayerFill = serde_json::from_str(old).unwrap();
        assert_eq!(fill.gradient().unwrap().repeat, GradientRepeat::None);
        assert!(!fill.gradient().unwrap().reverse);
    }

    #[test]
    fn styles_round_trip_as_json() {
        let style = LayerStyle {
            fill: None,
            border: Some(Border::default()),
            impasto: None,
            lightness_map: false,
        };
        let json = serde_json::to_string(&style).unwrap();
        assert_eq!(serde_json::from_str::<LayerStyle>(&json).unwrap(), style);
        // Absent fields read as none.
        assert_eq!(
            serde_json::from_str::<LayerStyle>("{}").unwrap(),
            LayerStyle::default()
        );
        assert_eq!(style.reach(), 5);
    }
}
