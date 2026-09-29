//! Compositing: layers (and the folder/mask tree) blended into the
//! pixels the view, export and thumbnails show.

use rayon::prelude::*;
use std::collections::HashMap;

use eframe::egui::{Color32, ColorImage, Rgba};

use super::{Canvas, Layer, LayerId, LayerKind, RowTileCache, layer_tile};
use crate::canvas::blend::{
    alpha_over_batch, average_over, color32_to_linear, color32s_to_linear, gamma_color32_to_rgba,
    rgba_to_color32_fast,
};
use crate::canvas::blend_modes::{
    BlendSpace, LayerBlend, composite as blend_composite, pixel_noise,
};

/// A precomputed composite of the layers below `first_layer` for one tile.
#[derive(Clone, Copy)]
pub struct BelowComposite<'a> {
    pub first_layer: usize,
    pub pixels: &'a [Rgba],
}

/// Tiles read shrunk for a quick preview (see
/// [`Canvas::write_tile_preview`]): each `block`×`block` square of the
/// tile's `w`×`h` pixels on the canvas averaged into one.
#[derive(Clone, Copy)]
struct Shrink<'a> {
    block: usize,
    w: usize,
    h: usize,
    /// One layer's pixels given already shrunk (a filter's preview), by
    /// layer index.
    layer: Option<(usize, &'a [Color32])>,
}

/// `data` (a `tile_size`-wide tile) averaged over each `block`×`block`
/// square of its top-left `w`×`h` pixels (premultiplied values, rounded):
/// `w.div_ceil(block)` × `h.div_ceil(block)` pixels.
pub(crate) fn shrink_tile(
    data: &[Color32],
    tile_size: usize,
    w: usize,
    h: usize,
    block: usize,
) -> Vec<Color32> {
    let (sw, sh) = (w.div_ceil(block), h.div_ceil(block));
    let mut out = Vec::with_capacity(sw * sh);
    for sy in 0..sh {
        let ys = sy * block..((sy + 1) * block).min(h);
        for sx in 0..sw {
            let xs = sx * block..((sx + 1) * block).min(w);
            let mut sum = [0u32; 4];
            for y in ys.clone() {
                for px in &data[y * tile_size + xs.start..y * tile_size + xs.end] {
                    for (s, v) in sum.iter_mut().zip(px.to_array()) {
                        *s += v as u32;
                    }
                }
            }
            let n = (ys.len() * xs.len()) as u32;
            let [r, g, b, a] = sum.map(|v| ((v + n / 2) / n) as u8);
            out.push(Color32::from_rgba_premultiplied(r, g, b, a));
        }
    }
    out
}

/// Which layers a fill or the magic wand looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleLayers {
    /// This one layer.
    Layer(usize),
    /// Everything visible (drafts aside).
    AllVisible,
    /// The layers marked as reference.
    Reference,
}

/// One visible layer's contribution to a tile, already in linear light.
struct LayerInput {
    opacity: f32,
    /// Used where the tile has no pixel data (background clear color, else transparent).
    fill: Rgba,
    linear: Option<Vec<Rgba>>,
}

/// A node of the per-tile composite tree used when folders or masks exist.
enum CompositeNode {
    Layer {
        input: LayerInput,
        mask: Option<MaskInput>,
        blend: LayerBlend,
    },
    /// Children are composited on their own first, then blended as one
    /// layer with the folder's opacity and blend mode.
    Group {
        opacity: f32,
        children: Vec<CompositeNode>,
        blend: LayerBlend,
    },
    /// An adjustment layer: changes what's composited so far, as much as
    /// its opacity (and mask) say.
    Adjust {
        filter: crate::canvas::filters::Filter,
        /// Its per-channel table, if it has one (built once per tile).
        lut: Option<Box<crate::canvas::filters::ChannelLut>>,
        opacity: f32,
        mask: Option<MaskInput>,
        space: BlendSpace,
    },
    /// A layer with clipping masks over it: the clipped layers are
    /// composited over the base's colour made opaque, and the result takes
    /// the base's alpha, then blends with the base's blend mode.
    Clip {
        base: Box<CompositeNode>,
        clipped: Vec<CompositeNode>,
        blend: LayerBlend,
    },
}

impl CompositeNode {
    fn blend(&self) -> LayerBlend {
        match self {
            CompositeNode::Layer { blend, .. }
            | CompositeNode::Group { blend, .. }
            | CompositeNode::Clip { blend, .. } => *blend,
            CompositeNode::Adjust { .. } => LayerBlend::Normal,
        }
    }

    /// Clip `node` to this one (which becomes, or already is, a clip group).
    fn clip(&mut self, node: CompositeNode) {
        if let CompositeNode::Clip { clipped, .. } = self {
            clipped.push(node);
            return;
        }
        let placeholder = CompositeNode::Group {
            opacity: 0.0,
            children: Vec::new(),
            blend: LayerBlend::Normal,
        };
        let base = std::mem::replace(self, placeholder);
        *self = CompositeNode::Clip {
            blend: base.blend(),
            base: Box::new(base),
            clipped: vec![node],
        };
    }
}

/// A mask's coverage (0..=1) per pixel of one tile; `None` means the tile
/// has no mask data, which shows everything.
struct MaskInput {
    values: Option<Vec<f32>>,
}

/// Normal-mode "over" of stored (gamma) values: the same operations as the
/// tree compositor's gamma path, so both give identical pixels.
#[inline]
pub(crate) fn gamma_over(src: Color32, dst: Color32) -> Color32 {
    crate::canvas::blend::GammaReader::new().over(src, dst)
}

/// Mask coverage of a (premultiplied) mask pixel: brightness times alpha,
/// so white shows, and black or transparent hides.
#[inline]
fn mask_coverage(px: Color32) -> f32 {
    (px.r() as f32 + px.g() as f32 + px.b() as f32) / (3.0 * 255.0)
}

/// Composite a tree onto `composite` for one pixel, each node with its
/// blend mode. `noise` is the pixel's Dissolve threshold.
fn composite_nodes(nodes: &[CompositeNode], idx: usize, mut composite: Rgba, noise: f32) -> Rgba {
    for node in nodes {
        // Paint layers, the common case, stay in this loop; the rest (folders,
        // clip groups, adjustments) go out of line.
        let CompositeNode::Layer { input, mask, blend } = node else {
            composite = composite_other(node, idx, composite, noise);
            continue;
        };
        let mut src = input.linear.as_ref().map_or(input.fill, |data| data[idx]);
        if src.a() == 0.0 {
            continue;
        }
        if input.opacity < 1.0 {
            src = src * input.opacity;
        }
        if let Some(mask) = mask {
            let coverage = mask.values.as_ref().map_or(1.0, |v| v[idx]);
            if coverage <= 0.0 {
                continue;
            }
            if coverage < 1.0 {
                src = src * coverage;
            }
        }
        composite = blend_composite(*blend, src, composite, noise);
    }
    composite
}

/// A folder, clip group or adjustment layer over `composite` (one pixel).
#[inline(never)]
fn composite_other(node: &CompositeNode, idx: usize, composite: Rgba, noise: f32) -> Rgba {
    let (src, blend) = match node {
        CompositeNode::Layer { .. } => {
            return composite_nodes(std::slice::from_ref(node), idx, composite, noise);
        }
        CompositeNode::Group {
            opacity,
            children,
            blend,
        } => {
            let inner = composite_nodes(children, idx, Rgba::TRANSPARENT, noise);
            if inner.a() == 0.0 {
                return composite;
            }
            let inner = if *opacity < 1.0 {
                inner * *opacity
            } else {
                inner
            };
            (inner, *blend)
        }
        CompositeNode::Adjust {
            filter,
            lut,
            opacity,
            mask,
            space,
        } => {
            return adjust(
                filter,
                lut.as_deref(),
                *opacity,
                mask.as_ref(),
                *space,
                idx,
                composite,
            );
        }
        CompositeNode::Clip {
            base,
            clipped,
            blend,
        } => match clip(base, clipped, idx, noise) {
            Some(inner) => (inner, *blend),
            None => return composite,
        },
    };
    blend_composite(blend, src, composite, noise)
}

// The rare nodes out of line, so the loop the common ones take stays small
// enough to be optimised as it was.

/// An adjustment layer applied to `composite` (one pixel).
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn adjust(
    filter: &crate::canvas::filters::Filter,
    lut: Option<&crate::canvas::filters::ChannelLut>,
    opacity: f32,
    mask: Option<&MaskInput>,
    space: BlendSpace,
    idx: usize,
    composite: Rgba,
) -> Rgba {
    let coverage = mask.map_or(1.0, |m| m.values.as_ref().map_or(1.0, |v| v[idx]));
    let amount = opacity * coverage;
    if amount <= 0.0 || composite.a() <= 0.0 {
        return composite;
    }
    let after = match space {
        BlendSpace::Linear => {
            color32_to_linear(filter.adjust(rgba_to_color32_fast(composite), lut))
        }
        // Gamma space holds sRGB values times alpha: the filter works on
        // them directly, with no trip through 8-bit colour.
        BlendSpace::Gamma => {
            let a = composite.a().min(1.0);
            let inv = 1.0 / a;
            let rgb = [
                composite.r() * inv,
                composite.g() * inv,
                composite.b() * inv,
            ];
            let [r, g, b] = filter.adjust_rgb(rgb, lut);
            Rgba::from_rgba_premultiplied(r * a, g * a, b * a, a)
        }
    };
    composite * (1.0 - amount) + after * amount
}

/// A clip group for one pixel: the clipped layers over the base's colour
/// made opaque, scaled back by the base's alpha (keeping them inside it);
/// `None` where the base has no paint.
#[inline(never)]
fn clip(base: &CompositeNode, clipped: &[CompositeNode], idx: usize, noise: f32) -> Option<Rgba> {
    let b = composite_nodes(std::slice::from_ref(base), idx, Rgba::TRANSPARENT, noise);
    if b.a() <= 0.0 {
        return None;
    }
    let inv = 1.0 / b.a();
    let opaque = Rgba::from_rgba_premultiplied(b.r() * inv, b.g() * inv, b.b() * inv, 1.0);
    // Over an opaque backdrop the result stays opaque.
    Some(composite_nodes(clipped, idx, opaque, noise) * b.a())
}

/// Source-over composite of `layers` (bottom to top) onto `composite` for one pixel.
#[inline]
fn composite_pixel(layers: &[LayerInput], idx: usize, mut composite: Rgba) -> Rgba {
    for layer in layers {
        let src = layer.linear.as_ref().map_or(layer.fill, |data| data[idx]);
        if src.a() == 0.0 {
            continue;
        }
        let src = if layer.opacity < 1.0 {
            src * layer.opacity
        } else {
            src
        };
        composite = src + composite * (1.0 - src.a());
    }
    composite
}

impl Canvas {
    /// Composite a canvas region into a `ColorImage`, optionally downsampled by `step`.
    pub fn write_region_to_color_image(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        out: &mut ColorImage,
        step: usize,
    ) {
        if w == 0 || h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }

        let step = step.max(1);
        let dst_w = w.div_ceil(step);
        let dst_h = h.div_ceil(step);

        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }

        // Optimization: Check if the region is within a single tile
        let start_tx = x / self.tile_size;
        let start_ty = y / self.tile_size;
        let end_tx = (x.saturating_add(w).saturating_sub(1)) / self.tile_size;
        let end_ty = (y.saturating_add(h).saturating_sub(1)) / self.tile_size;

        if start_tx == end_tx
            && start_ty == end_ty
            && step == 1
            && self.try_write_single_tile_fast(
                start_tx as i32,
                start_ty as i32,
                x..x + w,
                y..y + h,
                out,
            )
        {
            return;
        }

        if start_tx == end_tx && start_ty == end_ty {
            self.write_single_tile_region(
                x,
                y,
                w,
                h,
                start_tx as i32,
                start_ty as i32,
                dst_w,
                dst_h,
                step,
                out,
                None,
            );
            return;
        }

        self.write_multi_tile_region(x, y, dst_w, dst_h, step, out);
    }

    /// The whole flattened picture, composited tile by tile in parallel
    /// (the same per-tile path the screen uses).
    pub fn flatten(&self) -> ColorImage {
        let (w, h, ts) = (self.width, self.height, self.tile_size);
        let mut img = ColorImage::new([w, h], Color32::TRANSPARENT);
        let tiles_w = w.div_ceil(ts);
        img.pixels
            .par_chunks_mut(w * ts)
            .enumerate()
            .for_each(|(ty, strip)| {
                let strip_h = strip.len() / w;
                let mut part = ColorImage::new([0, 0], Color32::TRANSPARENT);
                for tx in 0..tiles_w {
                    self.write_tile_to_color_image(tx, ty, &mut part, 1, None);
                    let [pw, ph] = part.size;
                    for row in 0..ph.min(strip_h) {
                        let dst = row * w + tx * ts;
                        strip[dst..dst + pw]
                            .copy_from_slice(&part.pixels[row * pw..(row + 1) * pw]);
                    }
                }
            });
        img
    }

    /// Composite one whole tile into `out` (downsampled by `step`), optionally
    /// starting from a precomputed composite of the layers below
    /// `below.first_layer` instead of compositing them again.
    pub fn write_tile_to_color_image(
        &self,
        tx: usize,
        ty: usize,
        out: &mut ColorImage,
        step: usize,
        below: Option<BelowComposite<'_>>,
    ) {
        let x = tx * self.tile_size;
        let y = ty * self.tile_size;
        let w = self.tile_size.min(self.width.saturating_sub(x));
        let h = self.tile_size.min(self.height.saturating_sub(y));
        if w == 0 || h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }
        let step = step.max(1);
        let dst_w = w.div_ceil(step);
        let dst_h = h.div_ceil(step);
        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }
        if step == 1
            && self.try_write_single_tile_fast(tx as i32, ty as i32, x..x + w, y..y + h, out)
        {
            return;
        }
        self.write_single_tile_region(
            x, y, w, h, tx as i32, ty as i32, dst_w, dst_h, step, out, below,
        );
    }

    /// Composite the tile-local rectangle `rect` (`[x0, y0, x1, y1)`, clipped
    /// to the canvas) of tile `(tx, ty)` into `out`, sized to the rectangle.
    /// Same as [`Self::write_tile_to_color_image`] at step 1, for the part of
    /// a tile a stroke actually changed.
    pub fn write_tile_rect_to_color_image(
        &self,
        tx: usize,
        ty: usize,
        rect: [usize; 4],
        out: &mut ColorImage,
        below: Option<BelowComposite<'_>>,
    ) {
        let x = tx * self.tile_size + rect[0];
        let y = ty * self.tile_size + rect[1];
        let w = (rect[2] - rect[0]).min(self.width.saturating_sub(x));
        let h = (rect[3] - rect[1]).min(self.height.saturating_sub(y));
        if w == 0 || h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }
        if out.size != [w, h] {
            out.size = [w, h];
            out.pixels.resize(w * h, Color32::TRANSPARENT);
        }
        if self.try_write_single_tile_fast(tx as i32, ty as i32, x..x + w, y..y + h, out) {
            return;
        }
        self.write_single_tile_region(x, y, w, h, tx as i32, ty as i32, w, h, 1, out, below);
    }

    /// A quick, approximate composite of tile `(tx, ty)` shrunk by `block`,
    /// for the screen while a setting is dragged: each layer's tile is
    /// averaged by `block` first (its stored values), and only the averages
    /// are composited, so the work shrinks with the block's area. `layer`
    /// gives one layer's pixels already shrunk (`(index, pixels)`), in place
    /// of its tile. The exact composite
    /// ([`Self::write_tile_rect_downsampled`]) follows once the drag ends.
    pub fn write_tile_preview(
        &self,
        tx: usize,
        ty: usize,
        block: usize,
        out: &mut ColorImage,
        layer: Option<(usize, &[Color32])>,
    ) {
        let (x, y) = (tx * self.tile_size, ty * self.tile_size);
        let w = self.tile_size.min(self.width.saturating_sub(x));
        let h = self.tile_size.min(self.height.saturating_sub(y));
        let block = block.max(1);
        let (dst_w, dst_h) = (w.div_ceil(block), h.div_ceil(block));
        out.size = [dst_w, dst_h];
        out.pixels.clear();
        out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        if dst_w == 0 || dst_h == 0 {
            return;
        }
        let layer = layer.filter(|(_, pixels)| pixels.len() == dst_w * dst_h);
        let shrink = Some(Shrink { block, w, h, layer });
        let (tx, ty) = (tx as i32, ty as i32);
        if self.needs_tree_compositing() {
            let nodes = self.tile_nodes(tx, ty, shrink);
            for (idx, px) in out.pixels.iter_mut().enumerate() {
                let (gx, gy) = (x + idx % dst_w * block, y + idx / dst_w * block);
                let c = composite_nodes(
                    &nodes,
                    idx,
                    Rgba::TRANSPARENT,
                    pixel_noise(gx as u32, gy as u32),
                );
                *px = self.encode_tree(c);
            }
        } else {
            let layers = self.tile_layer_inputs(tx, ty, 0..self.layers.len(), shrink);
            for (idx, px) in out.pixels.iter_mut().enumerate() {
                *px = rgba_to_color32_fast(composite_pixel(&layers, idx, Rgba::TRANSPARENT));
            }
        }
    }

    /// [`Self::write_tile_rect_to_color_image`] shrunk by `block` (a power of
    /// two, `rect` aligned to it): each output pixel is the linear-light
    /// average of its block's composites, computed directly instead of
    /// compositing at full resolution, rounding to 8 bits, then decoding
    /// again to average (the zoomed-out stroke preview's main cost).
    pub fn write_tile_rect_downsampled(
        &self,
        tx: usize,
        ty: usize,
        rect: [usize; 4],
        block: usize,
        out: &mut ColorImage,
        below: Option<BelowComposite<'_>>,
    ) {
        let x = tx * self.tile_size + rect[0];
        let y = ty * self.tile_size + rect[1];
        let w = (rect[2] - rect[0]).min(self.width.saturating_sub(x));
        let h = (rect[3] - rect[1]).min(self.height.saturating_sub(y));
        let block = block.max(1);
        let (dst_w, dst_h) = (w.div_ceil(block), h.div_ceil(block));
        if dst_w == 0 || dst_h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }
        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }
        if below.is_none()
            && self.try_write_downsampled_fast(tx as i32, ty as i32, x..x + w, y..y + h, block, out)
        {
            return;
        }
        // Any stack: the general compositor already averages composites in
        // linear light when stepping.
        self.write_single_tile_region(
            x, y, w, h, tx as i32, ty as i32, dst_w, dst_h, block, out, below,
        );
    }

    /// Downsampled counterpart of [`Self::try_write_single_tile_fast`]: the
    /// same background + one opaque paint layer case, blended per pixel and
    /// averaged per block in one pass.
    #[inline(never)]
    fn try_write_downsampled_fast(
        &self,
        tx: i32,
        ty: i32,
        x_range: std::ops::Range<usize>,
        y_range: std::ops::Range<usize>,
        block: usize,
        out: &mut ColorImage,
    ) -> bool {
        // Averages in linear light: linear documents only.
        if self.needs_tree_compositing() {
            return false;
        }
        let mut bg_visible = false;
        let mut paint_layer = None;
        for (idx, layer) in self.layers.iter().enumerate() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            if idx == 0 {
                bg_visible = true;
            } else if paint_layer.is_none() && layer.opacity >= 1.0 {
                paint_layer = Some(idx);
            } else {
                return false;
            }
        }
        let bg_arc = bg_visible
            .then(|| self.layer_tile_cell(0, tx, ty))
            .flatten();
        let bg_guard = bg_arc
            .as_ref()
            .map(|a| a.lock().unwrap_or_else(|e| e.into_inner()));
        let bg_data = bg_guard.as_ref().and_then(|g| g.data.as_ref());
        let paint_arc = paint_layer.and_then(|idx| self.layer_tile_cell(idx, tx, ty));
        let paint_guard = paint_arc
            .as_ref()
            .map(|a| a.lock().unwrap_or_else(|e| e.into_inner()));
        let paint_data = paint_guard
            .as_ref()
            .filter(|g| !g.is_empty)
            .and_then(|g| g.data.as_ref());

        let ts = self.tile_size;
        let (x0, y0) = (x_range.start % ts, y_range.start % ts);
        let (w, h) = (x_range.len(), y_range.len());
        let dst_w = out.size[0];
        for (oy, row) in out.pixels.chunks_mut(dst_w).enumerate() {
            let rows = oy * block..((oy + 1) * block).min(h);
            for (ox, px) in row.iter_mut().enumerate() {
                let cols = ox * block..((ox + 1) * block).min(w);
                let pixels = rows.clone().flat_map(|r| {
                    let base = (y0 + r) * ts + x0;
                    cols.clone().map(move |c| base + c)
                });
                *px = average_over(pixels.map(|i| {
                    let dst = if bg_visible {
                        bg_data.map_or(self.clear_color, |d| d[i])
                    } else {
                        Color32::TRANSPARENT
                    };
                    let src = paint_data.map_or(Color32::TRANSPARENT, |d| d[i]);
                    (src, dst)
                }));
            }
        }
        true
    }

    /// Linear, premultiplied composite of the visible layers below
    /// `layer_idx` for every pixel of one tile: exactly the value the
    /// compositor has accumulated just before reaching `layer_idx`.
    pub fn composite_below(&self, layer_idx: usize, tx: i32, ty: i32) -> Vec<Rgba> {
        let layers = self.tile_layer_inputs(tx, ty, 0..layer_idx.min(self.layers.len()), None);
        (0..self.tile_size * self.tile_size)
            .map(|idx| composite_pixel(&layers, idx, Rgba::TRANSPARENT))
            .collect()
    }

    /// Cache key for [`Self::composite_below`]: everything besides pixel
    /// content (which only the active layer's strokes change) it depends on.
    pub fn composite_below_key(&self, layer_idx: usize) -> (Color32, Vec<(LayerId, bool, u32)>) {
        let layers = self.layers[..layer_idx.min(self.layers.len())]
            .iter()
            .map(|l| (l.id, l.visible, l.opacity.to_bits()))
            .collect();
        (self.clear_color, layers)
    }

    /// The visible, non-empty layers in `range` for one tile, pre-converted to
    /// linear light.
    fn tile_layer_inputs(
        &self,
        tx: i32,
        ty: i32,
        range: std::ops::Range<usize>,
        shrink: Option<Shrink<'_>>,
    ) -> Vec<LayerInput> {
        range
            .filter(|&i| self.layers[i].kind == LayerKind::Paint)
            .filter_map(|i| self.layer_input(i, tx, ty, BlendSpace::Linear, shrink))
            .collect()
    }

    /// Layer `i`'s contribution to one tile, in `space` (linear light, or
    /// the stored sRGB values), or `None` if it contributes nothing (hidden,
    /// fully transparent or no content there).
    fn layer_input(
        &self,
        i: usize,
        tx: i32,
        ty: i32,
        space: BlendSpace,
        shrink: Option<Shrink<'_>>,
    ) -> Option<LayerInput> {
        let layer = &self.layers[i];
        if !(layer.visible && layer.opacity > 0.0) {
            return None;
        }
        if let Some(Shrink {
            layer: Some((given, pixels)),
            ..
        }) = shrink
            && given == i
        {
            return Some(self.shrunk_input(i, pixels, space));
        }
        let cell = layer_tile(layer, tx, ty);
        let guard = cell
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        // A missing background tile shows the clear color; any other missing tile is empty.
        if guard.as_ref().map_or(i != 0, |g| g.is_empty) {
            return None;
        }
        let convert = |c: Color32| match space {
            BlendSpace::Linear => color32_to_linear(c),
            BlendSpace::Gamma => gamma_color32_to_rgba(c),
        };
        let data = guard.as_ref().and_then(|g| g.data.as_deref());
        if let Some(shrink) = shrink {
            let shrunk =
                data.map(|d| shrink_tile(d, self.tile_size, shrink.w, shrink.h, shrink.block));
            return Some(match shrunk {
                Some(pixels) => self.shrunk_input(i, &pixels, space),
                None => LayerInput {
                    opacity: layer.opacity,
                    fill: if i == 0 {
                        convert(self.clear_color)
                    } else {
                        Rgba::TRANSPARENT
                    },
                    linear: None,
                },
            });
        }
        Some(LayerInput {
            opacity: layer.opacity,
            fill: if i == 0 {
                convert(self.clear_color)
            } else {
                Rgba::TRANSPARENT
            },
            linear: data.map(|data| match space {
                BlendSpace::Linear => color32s_to_linear(data),
                BlendSpace::Gamma => {
                    let gamma = crate::canvas::blend::GammaReader::new();
                    data.iter().map(|&c| gamma.read(c)).collect()
                }
            }),
        })
    }

    /// Layer `i`'s input from pixels already shrunk.
    fn shrunk_input(&self, i: usize, pixels: &[Color32], space: BlendSpace) -> LayerInput {
        let layer = &self.layers[i];
        LayerInput {
            opacity: layer.opacity,
            fill: Rgba::TRANSPARENT,
            linear: Some(match space {
                BlendSpace::Linear => color32s_to_linear(pixels),
                BlendSpace::Gamma => {
                    let gamma = crate::canvas::blend::GammaReader::new();
                    pixels.iter().map(|&c| gamma.read(c)).collect()
                }
            }),
        }
    }

    /// The composite tree (folders and masks) for one tile.
    fn tile_nodes(&self, tx: i32, ty: i32, shrink: Option<Shrink<'_>>) -> Vec<CompositeNode> {
        let masks: HashMap<LayerId, usize> = self
            .layers
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l.kind {
                LayerKind::Mask { owner } => Some((owner, i)),
                _ => None,
            })
            .collect();
        self.child_nodes(tx, ty, None, &masks, 0, shrink)
    }

    /// Nodes for the children of `parent`, bottom to top.
    fn child_nodes(
        &self,
        tx: i32,
        ty: i32,
        parent: Option<LayerId>,
        masks: &HashMap<LayerId, usize>,
        depth: usize,
        shrink: Option<Shrink<'_>>,
    ) -> Vec<CompositeNode> {
        let mut nodes: Vec<CompositeNode> = Vec::new();
        if depth > self.layers.len() {
            return nodes; // a parent cycle; never expected
        }
        /// What clipped layers clip to: nothing yet (they show as usual), a
        /// base showing nothing in this tile (neither do they), or a node.
        enum Base {
            None,
            Empty,
            Node(usize),
        }
        let mut base = Base::None;
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.parent != parent || matches!(layer.kind, LayerKind::Mask { .. }) {
                continue;
            }
            let node = if layer.visible && layer.opacity > 0.0 {
                self.layer_node(i, tx, ty, masks, depth, shrink)
            } else {
                None
            };
            if layer.clipped && !matches!(base, Base::None) {
                if let (Base::Node(b), Some(node)) = (&base, node) {
                    nodes[*b].clip(node);
                }
                continue;
            }
            base = match node {
                Some(node) => {
                    nodes.push(node);
                    Base::Node(nodes.len() - 1)
                }
                None => Base::Empty,
            };
        }
        nodes
    }

    /// Layer `i` (a folder or paint layer) as a node for one tile, or `None`
    /// when it shows nothing there.
    fn layer_node(
        &self,
        i: usize,
        tx: i32,
        ty: i32,
        masks: &HashMap<LayerId, usize>,
        depth: usize,
        shrink: Option<Shrink<'_>>,
    ) -> Option<CompositeNode> {
        let layer = &self.layers[i];
        match layer.kind {
            LayerKind::Mask { .. } => None,
            LayerKind::Group => {
                let children = self.child_nodes(tx, ty, Some(layer.id), masks, depth + 1, shrink);
                (!children.is_empty()).then_some(CompositeNode::Group {
                    opacity: layer.opacity,
                    children,
                    blend: layer.blend,
                })
            }
            LayerKind::Paint if layer.adjustment.is_some() => Some(CompositeNode::Adjust {
                filter: layer.adjustment?,
                lut: layer.adjustment?.channel_lut(),
                opacity: layer.opacity,
                mask: masks
                    .get(&layer.id)
                    .and_then(|&m| self.mask_input(m, tx, ty, shrink)),
                space: self.blend_space,
            }),
            LayerKind::Paint => {
                let input = self.layer_input(i, tx, ty, self.blend_space, shrink)?;
                let mask = masks
                    .get(&layer.id)
                    .and_then(|&m| self.mask_input(m, tx, ty, shrink));
                Some(CompositeNode::Layer {
                    input,
                    mask,
                    blend: layer.blend,
                })
            }
        }
    }

    /// Mask coverage for one tile, or `None` when the mask is disabled.
    fn mask_input(
        &self,
        mask_idx: usize,
        tx: i32,
        ty: i32,
        shrink: Option<Shrink<'_>>,
    ) -> Option<MaskInput> {
        let layer = &self.layers[mask_idx];
        if !layer.visible {
            return None;
        }
        let cell = layer_tile(layer, tx, ty);
        let guard = cell
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        let values = guard
            .as_ref()
            .and_then(|g| g.data.as_deref())
            .map(|data| match shrink {
                Some(s) => shrink_tile(data, self.tile_size, s.w, s.h, s.block)
                    .into_iter()
                    .map(mask_coverage)
                    .collect(),
                None => data.iter().copied().map(mask_coverage).collect(),
            });
        Some(MaskInput { values })
    }

    /// Composite a region that lies entirely within one tile. Used when
    /// `try_write_single_tile_fast`'s stricter fast path doesn't apply
    /// (several layers, partial opacity, or `step != 1` downsampling).
    // Out of line: inlined into its callers, it changed how their fast paths
    // compiled (measured: 4% more instructions in the zoomed-out preview).
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn write_single_tile_region(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        tx: i32,
        ty: i32,
        dst_w: usize,
        dst_h: usize,
        step: usize,
        out: &mut ColorImage,
        below: Option<BelowComposite<'_>>,
    ) {
        let first_layer = below.map_or(0, |b| b.first_layer);
        // Folders/masks need the tree compositor (never with a below-cache,
        // which assumes a plain stack).
        let nodes = (below.is_none() && self.needs_tree_compositing())
            .then(|| self.tile_nodes(tx, ty, None));
        let layers = if nodes.is_some() {
            Vec::new()
        } else {
            self.tile_layer_inputs(tx, ty, first_layer..self.layers.len(), None)
        };
        let start = |idx: usize| below.map_or(Rgba::TRANSPARENT, |b| b.pixels[idx]);
        let composite_at = |idx: usize, gx: usize, gy: usize| match &nodes {
            Some(nodes) => composite_nodes(
                nodes,
                idx,
                Rgba::TRANSPARENT,
                pixel_noise(gx as u32, gy as u32),
            ),
            None => composite_pixel(&layers, idx, start(idx)),
        };
        let encode = |c: Rgba| {
            if nodes.is_some() {
                self.encode_tree(c)
            } else {
                rgba_to_color32_fast(c)
            }
        };
        // Downsampling averages in linear light, as the GPU's mips do, so a
        // zoomed-out preview shows what the full-resolution upload will. A
        // gamma-space composite is decoded (through its 8-bit value, exactly
        // as the full-resolution path stores it) before it's averaged.
        let gamma_tree = nodes.is_some() && self.blend_space == BlendSpace::Gamma;
        let linear = |c: Rgba| {
            if gamma_tree {
                color32_to_linear(self.encode_tree(c))
            } else {
                c
            }
        };

        for dst_y in 0..dst_h {
            let global_y_start = y + dst_y * step;
            let row_start = dst_y * dst_w;

            for dst_x in 0..dst_w {
                let global_x_start = x + dst_x * step;

                if step == 1 {
                    let local_y = global_y_start % self.tile_size;
                    let local_x = global_x_start % self.tile_size;
                    let src_idx = local_y * self.tile_size + local_x;
                    let composite = composite_at(src_idx, global_x_start, global_y_start);
                    out.pixels[row_start + dst_x] = encode(composite);
                } else {
                    // Downsample: average the linear composites of the covered pixels.
                    let mut r_acc = 0.0;
                    let mut g_acc = 0.0;
                    let mut b_acc = 0.0;
                    let mut a_acc = 0.0;
                    let mut count = 0.0;

                    for sy in 0..step {
                        let global_y = global_y_start + sy;
                        if global_y >= y + h {
                            continue;
                        }
                        let local_y = global_y % self.tile_size;

                        for sx in 0..step {
                            let global_x = global_x_start + sx;
                            if global_x >= x + w {
                                continue;
                            }
                            let local_x = global_x % self.tile_size;
                            let src_idx = local_y * self.tile_size + local_x;
                            let sub_composite = linear(composite_at(src_idx, global_x, global_y));

                            r_acc += sub_composite.r();
                            g_acc += sub_composite.g();
                            b_acc += sub_composite.b();
                            a_acc += sub_composite.a();
                            count += 1.0;
                        }
                    }

                    if count > 0.0 {
                        let inv = 1.0 / count;
                        out.pixels[row_start + dst_x] =
                            rgba_to_color32_fast(Rgba::from_rgba_premultiplied(
                                r_acc * inv,
                                g_acc * inv,
                                b_acc * inv,
                                a_acc * inv,
                            ));
                    }
                }
            }
        }
    }

    /// Composite a region that spans multiple tiles, caching one decoded
    /// tile per layer per output row.
    fn write_multi_tile_region(
        &self,
        x: usize,
        y: usize,
        dst_w: usize,
        dst_h: usize,
        step: usize,
        out: &mut ColorImage,
    ) {
        if self.needs_tree_compositing() {
            self.write_multi_tile_region_tree(x, y, dst_w, dst_h, step, out);
            return;
        }
        let clear_color_linear = color32_to_linear(self.clear_color);
        // Optimization: Cache tiles and pre-convert to linear space
        for dst_y in 0..dst_h {
            let global_y = y + dst_y * step;
            let ty = (global_y / self.tile_size) as i32;
            let local_y = global_y % self.tile_size;

            // Cache tile Arc and converted linear data for this row
            // Tuple: (cached_tx, tile_arc, linear_tile_data, is_empty)
            let mut row_tile_cache: RowTileCache = Vec::with_capacity(self.layers.len());

            // Initialize cache with None for each layer
            for _ in 0..self.layers.len() {
                row_tile_cache.push(None);
            }

            let mut dst_x = 0;
            while dst_x < dst_w {
                let global_x = x + dst_x * step;
                let tx = (global_x / self.tile_size) as i32;
                let local_x = global_x % self.tile_size;

                let dst_start = dst_y * dst_w + dst_x;

                let mut composite = Rgba::from_rgba_premultiplied(0.0, 0.0, 0.0, 0.0);

                for (layer_idx, layer) in self.layers.iter().enumerate() {
                    if !layer.visible || layer.opacity <= 0.0 {
                        continue;
                    }

                    // Check if we need to fetch a different tile
                    let needs_lookup = row_tile_cache[layer_idx]
                        .as_ref()
                        .is_none_or(|(cached_tx, _, _, _)| *cached_tx != tx);

                    if needs_lookup {
                        // Drop old cache entry
                        row_tile_cache[layer_idx] = None;

                        // Fetch new tile and pre-convert to linear
                        if let Some(tile_arc) = self.layer_tile_cell(layer_idx, tx, ty) {
                            // Lock temporarily to read data
                            let guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                            let is_empty = guard.is_empty;

                            // Pre-convert entire tile to linear space for efficiency
                            let linear_data = guard.data.as_deref().map(color32s_to_linear);

                            // Release lock and cache the Arc with converted data
                            drop(guard);
                            row_tile_cache[layer_idx] = Some((tx, tile_arc, linear_data, is_empty));
                        }
                    }

                    // Skip if tile is empty or missing
                    let (is_empty, linear_data) =
                        if let Some((_, _, linear_data, is_empty)) = &row_tile_cache[layer_idx] {
                            (*is_empty, linear_data.as_ref())
                        } else if layer_idx == 0 {
                            (false, None) // Background uses clear_color
                        } else {
                            continue; // Non-background layer with no tile
                        };

                    if is_empty {
                        continue;
                    }

                    // Resolve Pixel in linear space
                    let src = if let Some(linear_tile) = linear_data {
                        let src_idx = local_y * self.tile_size + local_x;
                        linear_tile[src_idx]
                    } else if layer_idx == 0 {
                        clear_color_linear
                    } else {
                        Rgba::TRANSPARENT
                    };

                    if src.a() == 0.0 {
                        continue;
                    }

                    // Apply opacity and blend (already in linear space)
                    let src = if layer.opacity < 1.0 {
                        src * layer.opacity
                    } else {
                        src
                    };
                    composite = src + composite * (1.0 - src.a());
                }

                out.pixels[dst_start] = rgba_to_color32_fast(composite);
                dst_x += 1;
            }
        }
    }

    /// [`Self::write_multi_tile_region`] for canvases with folders or masks:
    /// same sampling, through the composite tree. Trees are cached for one
    /// row of tiles at a time to bound memory on large exports.
    fn write_multi_tile_region_tree(
        &self,
        x: usize,
        y: usize,
        dst_w: usize,
        dst_h: usize,
        step: usize,
        out: &mut ColorImage,
    ) {
        let mut cache: HashMap<i32, Vec<CompositeNode>> = HashMap::new();
        let mut cached_ty = None;
        for dst_y in 0..dst_h {
            let global_y = y + dst_y * step;
            let ty = (global_y / self.tile_size) as i32;
            if cached_ty != Some(ty) {
                cache.clear();
                cached_ty = Some(ty);
            }
            let local_y = global_y % self.tile_size;
            for dst_x in 0..dst_w {
                let global_x = x + dst_x * step;
                let tx = (global_x / self.tile_size) as i32;
                let local_x = global_x % self.tile_size;
                let nodes = cache
                    .entry(tx)
                    .or_insert_with(|| self.tile_nodes(tx, ty, None));
                let idx = local_y * self.tile_size + local_x;
                let noise = pixel_noise(global_x as u32, global_y as u32);
                let composite = composite_nodes(nodes, idx, Rgba::TRANSPARENT, noise);
                out.pixels[dst_y * dst_w + dst_x] = self.encode_tree(composite);
            }
        }
    }

    pub fn write_thumbnail_nearest(&self, max_edge: usize, out: &mut ColorImage) {
        let max_edge = max_edge.max(1);
        let longest = self.width.max(self.height).max(1);
        let scale = max_edge as f32 / longest as f32;
        let dst_w = ((self.width as f32 * scale).round() as usize).max(1);
        let dst_h = ((self.height as f32 * scale).round() as usize).max(1);

        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }

        let mut pixel = ColorImage::new([1, 1], Color32::TRANSPARENT);
        for y in 0..dst_h {
            let src_y = (y * self.height / dst_h).min(self.height.saturating_sub(1));
            for x in 0..dst_w {
                let src_x = (x * self.width / dst_w).min(self.width.saturating_sub(1));
                self.write_region_to_color_image(src_x, src_y, 1, 1, &mut pixel, 1);
                out.pixels[y * dst_w + x] = pixel.pixels[0];
            }
        }
    }

    pub(super) fn try_write_single_tile_fast(
        &self,
        tx: i32,
        ty: i32,
        x_range: std::ops::Range<usize>,
        y_range: std::ops::Range<usize>,
        out: &mut ColorImage,
    ) -> bool {
        // Gamma documents with plain Normal layers get this path too.
        if !self.is_plain_stack() {
            return false;
        }
        let gamma = self.blend_space == BlendSpace::Gamma;
        let reader = crate::canvas::blend::GammaReader::new();
        let mut bg_visible = false;
        let mut paint_layer = None;
        for (idx, layer) in self.layers.iter().enumerate() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            if idx == 0 {
                bg_visible = true;
            } else if paint_layer.is_none() && layer.opacity >= 1.0 {
                paint_layer = Some(idx);
            } else {
                return false;
            }
        }

        let bg_arc = bg_visible
            .then(|| self.layer_tile_cell(0, tx, ty))
            .flatten();
        let bg_guard = bg_arc
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        let bg_data = bg_guard.as_ref().and_then(|guard| guard.data.as_ref());

        let paint_arc = paint_layer.and_then(|idx| self.layer_tile_cell(idx, tx, ty));
        let paint_guard = paint_arc
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        let paint_data = paint_guard
            .as_ref()
            .filter(|guard| !guard.is_empty)
            .and_then(|guard| guard.data.as_ref());

        let w = x_range.len();
        let local_x = x_range.start % self.tile_size;
        let local_y = y_range.start % self.tile_size;

        // Blend in fixed-size chunks via the SIMD-batched alpha_over instead of one
        // call per pixel; alpha_over already returns `src`/`dst` exactly for the
        // alpha==255/0 cases internally, so no separate fast-path branch is needed.
        const CHUNK: usize = 64;
        let mut bg_buf = [Color32::TRANSPARENT; CHUNK];

        for row in 0..y_range.len() {
            let src_row = (local_y + row) * self.tile_size + local_x;
            let dst_row = row * w;

            let mut col = 0;
            while col < w {
                let n = CHUNK.min(w - col);
                for (k, slot) in bg_buf[..n].iter_mut().enumerate() {
                    *slot = if bg_visible {
                        bg_data.map_or(self.clear_color, |data| data[src_row + col + k])
                    } else {
                        Color32::TRANSPARENT
                    };
                }
                match paint_data {
                    Some(data) if gamma => {
                        let src = &data[src_row + col..src_row + col + n];
                        let dst = &mut out.pixels[dst_row + col..dst_row + col + n];
                        for ((o, &s), &b) in dst.iter_mut().zip(src).zip(&bg_buf[..n]) {
                            *o = reader.over(s, b);
                        }
                    }
                    Some(data) => {
                        alpha_over_batch(
                            &data[src_row + col..src_row + col + n],
                            &bg_buf[..n],
                            &mut out.pixels[dst_row + col..dst_row + col + n],
                        );
                    }
                    None => {
                        out.pixels[dst_row + col..dst_row + col + n].copy_from_slice(&bg_buf[..n]);
                    }
                }
                col += n;
            }
        }
        true
    }

    /// Pixels of layer `source` (everything visible when `None`) over the
    /// canvas rectangle `(x, y, w, h)`, row-major. Outside the canvas and
    /// unpainted tiles read as transparent.
    pub fn render_reference(
        &self,
        source: Option<usize>,
        x: i32,
        y: i32,
        w: usize,
        h: usize,
    ) -> Vec<Color32> {
        // Every visible paint layer, bottom first, with its effective opacity
        // (folders included). Blend modes and masks are ignored: this only
        // has to show where the lines are, and plain "over" is fast.
        let layers: Vec<(usize, f32)> = match source {
            Some(idx) => vec![(idx, 1.0)],
            None => self.reference_layers(),
        };
        self.render_layers(&layers, x, y, w, h)
    }

    /// [`Self::render_reference`] of the layers `source` names.
    pub fn render_sample(
        &self,
        source: SampleLayers,
        x: i32,
        y: i32,
        w: usize,
        h: usize,
    ) -> Vec<Color32> {
        match source {
            SampleLayers::Layer(idx) => self.render_reference(Some(idx), x, y, w, h),
            SampleLayers::AllVisible => self.render_reference(None, x, y, w, h),
            SampleLayers::Reference => self.render_marked_reference(x, y, w, h),
        }
    }

    /// [`Self::render_reference`] of the layers marked as reference (and
    /// those in folders marked so), shown or not, bottom first.
    pub fn render_marked_reference(&self, x: i32, y: i32, w: usize, h: usize) -> Vec<Color32> {
        let by_id: HashMap<LayerId, &Layer> = self.layers.iter().map(|l| (l.id, l)).collect();
        let layers: Vec<(usize, f32)> = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == LayerKind::Paint && l.adjustment.is_none())
            .filter(|(_, l)| {
                let mut current = Some(*l);
                while let Some(e) = current {
                    if e.reference {
                        return true;
                    }
                    current = e.parent.and_then(|p| by_id.get(&p).copied());
                }
                false
            })
            .map(|(i, _)| (i, 1.0))
            .collect();
        self.render_layers(&layers, x, y, w, h)
    }

    /// Whether any layer (or folder) is marked as reference.
    pub fn has_reference_layer(&self) -> bool {
        self.layers.iter().any(|l| l.reference)
    }

    /// `layers` (index, opacity), bottom first, "over" each other across
    /// the canvas rectangle `(x, y, w, h)`.
    fn render_layers(
        &self,
        layers: &[(usize, f32)],
        x: i32,
        y: i32,
        w: usize,
        h: usize,
    ) -> Vec<Color32> {
        let ts = self.tile_size as i32;
        let mut out = vec![Color32::TRANSPARENT; w * h];
        if w == 0 || h == 0 {
            return out;
        }
        let (x1, y1) = (x + w as i32, y + h as i32);
        let mut tile = vec![Color32::TRANSPARENT; (ts * ts) as usize];
        for ty in y.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
            for tx in x.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
                tile.fill(Color32::TRANSPARENT);
                let mut any = false;
                for &(idx, opacity) in layers {
                    let Some(cell) = self.layer_tile_cell(idx, tx, ty) else {
                        continue;
                    };
                    let cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(data) = cell.data.as_ref().filter(|_| !cell.is_empty) else {
                        continue;
                    };
                    if !any && opacity >= 1.0 {
                        tile.copy_from_slice(data);
                    } else {
                        for (d, &s) in tile.iter_mut().zip(data) {
                            let s = crate::canvas::blend::apply_opacity_scale(s, opacity);
                            *d = crate::canvas::blend::alpha_over(s, *d);
                        }
                    }
                    any = true;
                }
                if !any {
                    continue;
                }
                let (ox, oy) = (tx * ts, ty * ts);
                let (cx0, cx1) = (x.max(ox), x1.min(ox + ts));
                for py in y.max(oy)..y1.min(oy + ts) {
                    let src = ((py - oy) * ts + (cx0 - ox)) as usize;
                    let dst = ((py - y) as usize) * w + (cx0 - x) as usize;
                    let n = (cx1 - cx0) as usize;
                    out[dst..dst + n].copy_from_slice(&tile[src..src + n]);
                }
            }
        }
        out
    }

    /// Paint layers that show, bottom first, with opacity multiplied down
    /// through their folders. Drafts (and what's in draft folders) are left
    /// out.
    fn reference_layers(&self) -> Vec<(usize, f32)> {
        let by_id: HashMap<LayerId, &Layer> = self.layers.iter().map(|l| (l.id, l)).collect();
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == LayerKind::Paint && l.visible && !l.draft)
            .filter_map(|(i, l)| {
                let mut opacity = l.opacity;
                let mut parent = l.parent;
                while let Some(p) = parent.and_then(|id| by_id.get(&id)) {
                    if !p.visible || p.draft {
                        return None;
                    }
                    opacity *= p.opacity;
                    parent = p.parent;
                }
                (opacity > 0.0).then_some((i, opacity))
            })
            .collect()
    }
}
