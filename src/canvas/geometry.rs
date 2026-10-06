//! Whole-document geometry (the Image menu): canvas size and crop, image
//! size, rotating and flipping. Pure functions over one layer's pixels as
//! a row-major buffer; [`Canvas::apply_image_op`] runs them on every layer.

use eframe::egui::Color32;

use crate::canvas::Canvas;
use crate::canvas::storage::{DocumentState, LayerKind};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImageOp {
    /// The new canvas shows the old one's `w`×`h` area at `(x, y)` (which
    /// may reach past its edges): crop, or canvas size with an anchor.
    Reframe {
        x: i32,
        y: i32,
        w: usize,
        h: usize,
    },
    /// Resample every layer to `w`×`h`; `smooth` false keeps hard pixels.
    Resize {
        w: usize,
        h: usize,
        smooth: bool,
    },
    RotateCw,
    RotateCcw,
    Rotate180,
    FlipHorizontal,
    FlipVertical,
}

impl ImageOp {
    /// The canvas size after this, from `w`×`h`.
    pub fn new_size(self, w: usize, h: usize) -> (usize, usize) {
        match self {
            ImageOp::Reframe { w, h, .. } | ImageOp::Resize { w, h, .. } => (w, h),
            ImageOp::RotateCw | ImageOp::RotateCcw => (h, w),
            _ => (w, h),
        }
    }

    /// One layer's `w`×`h` pixels after this; `fill` is what new area
    /// shows (the background colour, a mask's white, else transparent).
    pub fn apply(self, src: &[Color32], w: usize, h: usize, fill: Color32) -> Vec<Color32> {
        let (nw, nh) = self.new_size(w, h);
        match self {
            ImageOp::Reframe { x, y, .. } => {
                let mut out = vec![fill; nw * nh];
                for (ny, row) in out.chunks_mut(nw).enumerate() {
                    let sy = y + ny as i32;
                    if sy < 0 || sy >= h as i32 {
                        continue;
                    }
                    let (sx0, sx1) = (x.max(0), (x + nw as i32).min(w as i32));
                    if sx1 <= sx0 {
                        continue;
                    }
                    let src_row = &src[sy as usize * w..][..w];
                    row[(sx0 - x) as usize..(sx1 - x) as usize]
                        .copy_from_slice(&src_row[sx0 as usize..sx1 as usize]);
                }
                out
            }
            ImageOp::Resize { smooth, .. } => resample(src, w, h, nw, nh, smooth),
            _ => {
                let mut out = vec![fill; nw * nh];
                for (i, o) in out.iter_mut().enumerate() {
                    let (nx, ny) = (i % nw, i / nw);
                    let (sx, sy) = match self {
                        ImageOp::RotateCw => (ny, h - 1 - nx),
                        ImageOp::RotateCcw => (w - 1 - ny, nx),
                        ImageOp::Rotate180 => (w - 1 - nx, h - 1 - ny),
                        ImageOp::FlipHorizontal => (w - 1 - nx, ny),
                        _ => (nx, h - 1 - ny),
                    };
                    *o = src[sy * w + sx];
                }
                out
            }
        }
    }
}

/// Premultiplied pixels resampled (Catmull-Rom, or nearest for hard pixels).
fn resample(
    src: &[Color32],
    w: usize,
    h: usize,
    nw: usize,
    nh: usize,
    smooth: bool,
) -> Vec<Color32> {
    if !smooth {
        return nearest(src, w, h, nw, nh);
    }
    let bytes: Vec<u8> = src.iter().flat_map(|c| c.to_array()).collect();
    let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) else {
        return vec![Color32::TRANSPARENT; nw * nh];
    };
    let filter = if smooth {
        image::imageops::FilterType::CatmullRom
    } else {
        image::imageops::FilterType::Nearest
    };
    image::imageops::resize(&img, nw as u32, nh as u32, filter)
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            Color32::from_rgba_premultiplied(r, g, b, a)
        })
        .collect()
}

/// Hard pixels: each output pixel copies the source pixel under its centre
/// (rows in parallel; the image crate's general resampler is ~10× slower).
fn nearest(src: &[Color32], w: usize, h: usize, nw: usize, nh: usize) -> Vec<Color32> {
    use rayon::prelude::*;
    let xs: Vec<usize> = (0..nw)
        .map(|x| (((x as f64 + 0.5) * w as f64 / nw as f64) as usize).min(w - 1))
        .collect();
    let mut out = vec![Color32::TRANSPARENT; nw * nh];
    out.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
        let sy = (((y as f64 + 0.5) * h as f64 / nh as f64) as usize).min(h - 1);
        let src_row = &src[sy * w..(sy + 1) * w];
        for (o, &sx) in row.iter_mut().zip(&xs) {
            *o = src_row[sx];
        }
    });
    out
}

impl Canvas {
    /// Apply `op` to every layer. Returns the document as it was, for undo
    /// (swap it back with [`Canvas::swap_document`]).
    pub fn apply_image_op(&mut self, op: ImageOp) -> DocumentState {
        let (w, h) = (self.width(), self.height());
        let (nw, nh) = op.new_size(w, h);
        let ts = self.tile_size();
        let clear = self.clear_color();
        let mut new_layers = Vec::with_capacity(self.layers.len());
        for (idx, layer) in self.layers.iter().enumerate() {
            let fill = if idx == 0 {
                clear
            } else if matches!(layer.kind, LayerKind::Mask { .. }) {
                Color32::WHITE
            } else {
                Color32::TRANSPARENT
            };
            // Folders have no pixels; an empty layer stays empty (a bare
            // background shows its colour everywhere either way).
            let tiles = if layer.kind == LayerKind::Group || self.layer_tile_keys(idx).is_empty() {
                Vec::new()
            } else {
                let src = self.layer_pixels(idx, fill);
                let out = op.apply(&src, w, h, fill);
                split_tiles(&out, nw, nh, ts, fill)
            };
            let mut shell = layer.shell();
            // Text keeps its source when only the frame moves; resampled,
            // turned or flipped, it's plain pixels.
            shell.text = match op {
                ImageOp::Reframe { x, y, .. } => shell.text.take().map(|mut t| {
                    t.pos -= eframe::egui::vec2(x as f32, y as f32);
                    t
                }),
                _ => None,
            };
            // Impasto heights move as the paint does (picked, not blended:
            // a height is two bytes).
            shell.height = shell.height.take().map(|map| {
                let heights: Vec<Color32> = {
                    let mut all = vec![0u16; w * h];
                    for ((tx, ty), tile) in map.tiles() {
                        for (y, row) in tile.chunks(ts).enumerate() {
                            let gy = ty as usize * ts + y;
                            let gx = tx as usize * ts;
                            if gy >= h || gx >= w {
                                continue;
                            }
                            let n = row.len().min(w - gx);
                            all[gy * w + gx..gy * w + gx + n].copy_from_slice(&row[..n]);
                        }
                    }
                    crate::canvas::impasto::to_pixels(&all)
                };
                let op = match op {
                    ImageOp::Resize { w, h, .. } => ImageOp::Resize {
                        w,
                        h,
                        smooth: false,
                    },
                    other => other,
                };
                let zero = crate::canvas::impasto::to_pixels(&[0])[0];
                let moved = crate::canvas::impasto::from_pixels(&op.apply(&heights, w, h, zero));
                let out = crate::canvas::impasto::HeightMap::default();
                for ty in 0..nh.div_ceil(ts) {
                    for tx in 0..nw.div_ceil(ts) {
                        let mut tile = vec![0u16; ts * ts];
                        for y in 0..ts.min(nh - ty * ts) {
                            let gy = ty * ts + y;
                            let n = ts.min(nw - tx * ts);
                            tile[y * ts..y * ts + n]
                                .copy_from_slice(&moved[gy * nw + tx * ts..gy * nw + tx * ts + n]);
                        }
                        out.set_tile((tx as i32, ty as i32), Some(tile));
                    }
                }
                Box::new(out)
            });
            // Vector lines likewise; a gradient fill's ends move with the
            // frame too.
            shell.vector = match op {
                ImageOp::Reframe { x, y, .. } => shell.vector.take().map(|mut v| {
                    for s in &mut v.strokes {
                        for p in &mut s.points {
                            p[0] -= x as f32;
                            p[1] -= y as f32;
                        }
                    }
                    v
                }),
                _ => None,
            };
            if let (
                ImageOp::Reframe { x, y, .. },
                Some(crate::canvas::layer_style::LayerFill::Gradient { start, end, .. }),
            ) = (op, shell.style.fill.as_mut())
            {
                for p in [start, end] {
                    p[0] -= x as f32;
                    p[1] -= y as f32;
                }
            }
            new_layers.push((shell, tiles));
        }
        let mut doc = DocumentState {
            width: nw,
            height: nh,
            layers: Vec::new(),
            active_layer_idx: self.active_layer_idx,
        };
        for (layer, tiles) in new_layers {
            for ((tx, ty), data) in tiles {
                layer.set_tile(tx, ty, data);
            }
            doc.layers.push(layer);
        }
        self.swap_document(&mut doc);
        doc
    }

    /// Layer `idx` over the whole canvas, row-major; unpainted areas read
    /// as `fill`.
    fn layer_pixels(&self, idx: usize, fill: Color32) -> Vec<Color32> {
        let (w, h, ts) = (self.width(), self.height(), self.tile_size());
        let mut out = vec![fill; w * h];
        for ty in 0..h.div_ceil(ts) {
            for tx in 0..w.div_ceil(ts) {
                let Some(data) = self.get_layer_tile_data(idx, tx as i32, ty as i32) else {
                    continue;
                };
                let (x0, y0) = (tx * ts, ty * ts);
                let n = ts.min(w - x0);
                for ly in 0..ts.min(h - y0) {
                    out[(y0 + ly) * w + x0..][..n].copy_from_slice(&data[ly * ts..][..n]);
                }
            }
        }
        out
    }
}

/// A `w`×`h` buffer cut into tiles, leaving out those that are all `fill`.
fn split_tiles(
    src: &[Color32],
    w: usize,
    h: usize,
    ts: usize,
    fill: Color32,
) -> Vec<((i32, i32), Vec<Color32>)> {
    let mut tiles = Vec::new();
    for ty in 0..h.div_ceil(ts) {
        for tx in 0..w.div_ceil(ts) {
            let mut data = vec![fill; ts * ts];
            let (x0, y0) = (tx * ts, ty * ts);
            let n = ts.min(w - x0);
            for ly in 0..ts.min(h - y0) {
                data[ly * ts..][..n].copy_from_slice(&src[(y0 + ly) * w + x0..][..n]);
            }
            if data.iter().any(|&p| p != fill) {
                tiles.push(((tx as i32, ty as i32), data));
            }
        }
    }
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3×2: pixel value = its index.
    fn grid() -> Vec<Color32> {
        (0..6).map(|i| Color32::from_gray(i * 10)).collect()
    }

    fn values(px: &[Color32]) -> Vec<u8> {
        px.iter().map(|c| c.r() / 10).collect()
    }

    #[test]
    fn rotations_and_flips_move_pixels_where_they_should() {
        let g = grid(); // 0 1 2 / 3 4 5
        let f = Color32::TRANSPARENT;
        assert_eq!(
            values(&ImageOp::RotateCw.apply(&g, 3, 2, f)),
            [3, 0, 4, 1, 5, 2]
        );
        assert_eq!(
            values(&ImageOp::RotateCcw.apply(&g, 3, 2, f)),
            [2, 5, 1, 4, 0, 3]
        );
        assert_eq!(
            values(&ImageOp::Rotate180.apply(&g, 3, 2, f)),
            [5, 4, 3, 2, 1, 0]
        );
        assert_eq!(
            values(&ImageOp::FlipHorizontal.apply(&g, 3, 2, f)),
            [2, 1, 0, 5, 4, 3]
        );
        assert_eq!(
            values(&ImageOp::FlipVertical.apply(&g, 3, 2, f)),
            [3, 4, 5, 0, 1, 2]
        );
    }

    #[test]
    fn reframe_crops_and_pads_with_the_fill() {
        let g = grid();
        let crop = ImageOp::Reframe {
            x: 1,
            y: 0,
            w: 2,
            h: 2,
        }
        .apply(&g, 3, 2, Color32::RED);
        assert_eq!(values(&crop), [1, 2, 4, 5]);
        let grown = ImageOp::Reframe {
            x: -1,
            y: 0,
            w: 5,
            h: 2,
        }
        .apply(&g, 3, 2, Color32::RED);
        assert_eq!(grown[0], Color32::RED);
        assert_eq!(grown[4], Color32::RED);
        assert_eq!(values(&grown[1..4]), [0, 1, 2]);
    }

    #[test]
    fn nearest_resize_keeps_hard_pixels() {
        let g = grid();
        let big = ImageOp::Resize {
            w: 6,
            h: 4,
            smooth: false,
        }
        .apply(&g, 3, 2, Color32::TRANSPARENT);
        assert_eq!(values(&big[..6]), [0, 0, 1, 1, 2, 2]);
    }

    #[test]
    fn hard_pixel_resizes_copy_whole_pixels() {
        let g = grid(); // 0 1 2 / 3 4 5
        let one = ImageOp::Resize {
            w: 1,
            h: 1,
            smooth: false,
        };
        assert_eq!(
            values(&one.apply(&g, 3, 2, Color32::TRANSPARENT)),
            [4],
            "the centre pixel"
        );
        let tall = ImageOp::Resize {
            w: 3,
            h: 4,
            smooth: false,
        };
        assert_eq!(
            values(&tall.apply(&g, 3, 2, Color32::TRANSPARENT)),
            [0, 1, 2, 0, 1, 2, 3, 4, 5, 3, 4, 5]
        );
    }

    #[test]
    fn a_rotated_canvas_undoes_to_the_original() {
        let mut canvas = Canvas::new(100, 60, Color32::WHITE, 64);
        let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
        tile[3 * 64 + 5] = Color32::RED;
        canvas.set_layer_tile_data(1, 0, 0, tile);
        let mut before = canvas.apply_image_op(ImageOp::RotateCw);
        assert_eq!((canvas.width(), canvas.height()), (60, 100));
        // (5, 3) on a 60-wide canvas turned clockwise lands at (60-1-3, 5).
        let t = canvas.get_layer_tile_data(1, 0, 0).unwrap();
        assert_eq!(t[5 * 64 + 56], Color32::RED);
        canvas.swap_document(&mut before);
        assert_eq!((canvas.width(), canvas.height()), (100, 60));
        let t = canvas.get_layer_tile_data(1, 0, 0).unwrap();
        assert_eq!(t[3 * 64 + 5], Color32::RED);
    }

    #[test]
    fn growing_the_canvas_extends_the_background_colour() {
        let mut canvas = Canvas::new(64, 64, Color32::BLUE, 64);
        canvas.apply_image_op(ImageOp::Reframe {
            x: -64,
            y: 0,
            w: 128,
            h: 64,
        });
        let mut img = eframe::egui::ColorImage::new([1, 1], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(10, 10, 1, 1, &mut img, 1);
        assert_eq!(img.pixels[0], Color32::BLUE);
    }
}
