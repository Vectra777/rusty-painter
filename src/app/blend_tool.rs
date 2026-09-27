//! Smudge and Blur: blending tools that work with the normal brush's size,
//! hardness, spacing, flow, opacity and pressure settings, but move or
//! soften the paint already on the layer instead of adding colour (like
//! Clip Studio's Blend tools).
//!
//! Smudge carries a patch of paint along the stroke: every dab mixes the
//! carried paint into the canvas under the tip, then picks up some of the
//! result (how much it keeps is the smudge length). Blur mixes each pixel
//! toward the average around it. Both are sequential per dab (each dab sees
//! the previous one's result), so they run their own small engine rather
//! than the batched brush pipeline.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::canvas::history::{TileSnapshot, UndoAction};
use crate::canvas::storage::{LayerId, LayerKind};
use eframe::egui::{Color32, Vec2};
use std::collections::HashMap;

pub struct BlendToolSettings {
    /// Smudge: how much of the carried paint stays with the brush each dab
    /// (0 = barely drags, 1 = smears a colour a long way).
    pub smudge_length: f32,
    /// Blur: size of the area averaged, relative to the brush radius.
    pub blur_size: f32,
}

impl Default for BlendToolSettings {
    fn default() -> Self {
        Self {
            smudge_length: 0.8,
            blur_size: 0.35,
        }
    }
}

/// A patch of carried paint (linear premultiplied, 0..1 per channel).
struct Carry {
    side: usize,
    px: Vec<[f32; 4]>,
}

pub struct BlendStroke {
    layer_id: LayerId,
    smudge: bool,
    /// Tiles as they were before the stroke first changed them.
    before: HashMap<(i32, i32), Vec<Color32>>,
    last: Option<Vec2>,
    /// Distance travelled since the last dab.
    travelled: f32,
    /// Smudge: the paint each mirror copy carries (copy 0 is the stroke
    /// itself).
    carries: Vec<Option<Carry>>,
    /// Mirror painting for this stroke.
    symmetry: crate::brush_engine::symmetry::Symmetry,
    copies: Vec<crate::brush_engine::symmetry::Copy2>,
}

/// Pixels are mixed as linear-light premultiplied colour, the same space
/// the compositor works in. (Mixing the stored sRGB bytes and clamping each
/// channel to alpha darkened light colours at soft edges, as if another
/// colour were being picked up.)
fn to_f(c: Color32) -> [f32; 4] {
    let l = crate::canvas::blend::color32_to_linear(c);
    [l.r(), l.g(), l.b(), l.a()]
}

fn to_c(v: [f32; 4]) -> Color32 {
    let a = v[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return Color32::TRANSPARENT;
    }
    // Premultiplied: colour can't exceed alpha.
    let c = |x: f32| x.clamp(0.0, a);
    crate::canvas::blend::rgba_to_color32_fast(eframe::egui::Rgba::from_rgba_premultiplied(
        c(v[0]),
        c(v[1]),
        c(v[2]),
        a,
    ))
}

/// Box blur of a `side`×`side` patch with radius `r` (two passes, so it
/// looks nearly Gaussian).
fn box_blur(src: &[[f32; 4]], side: usize, r: usize) -> Vec<[f32; 4]> {
    let pass = |input: &[[f32; 4]], horizontal: bool| -> Vec<[f32; 4]> {
        let mut out = vec![[0.0; 4]; side * side];
        for line in 0..side {
            let at = |i: usize| {
                if horizontal {
                    line * side + i
                } else {
                    i * side + line
                }
            };
            let mut sum = [0.0f32; 4];
            let mut count = 0.0f32;
            for i in 0..=r.min(side - 1) {
                let p = input[at(i)];
                for c in 0..4 {
                    sum[c] += p[c];
                }
                count += 1.0;
            }
            for i in 0..side {
                out[at(i)] = sum.map(|s| s / count);
                let add = i + r + 1;
                if add < side {
                    let p = input[at(add)];
                    for c in 0..4 {
                        sum[c] += p[c];
                    }
                    count += 1.0;
                }
                if i >= r {
                    let p = input[at(i - r)];
                    for c in 0..4 {
                        sum[c] -= p[c];
                    }
                    count -= 1.0;
                }
            }
        }
        out
    };
    let mut v = src.to_vec();
    for _ in 0..2 {
        v = pass(&v, true);
        v = pass(&v, false);
    }
    v
}

impl PainterApp {
    pub(crate) fn set_blend_tool(&mut self, smudge: bool) {
        // They use the brush's settings, not the eraser's.
        self.set_brush_tool(false);
        self.active_tool = if smudge { Tool::Smudge } else { Tool::Blur };
    }

    pub(crate) fn blend_press(&mut self, pos: Vec2, pressure: f32) {
        let idx = self.canvas.active_layer_idx;
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        if layer.locked || matches!(layer.kind, LayerKind::Group) {
            return;
        }
        let layer_id = layer.id;
        self.release_canvas();
        // Like a brush stroke: a second press ends the running one first.
        self.blend_release();
        self.mark_action();
        self.brush_state.blend_stroke = Some(BlendStroke {
            layer_id,
            smudge: matches!(self.active_tool, Tool::Smudge),
            before: HashMap::new(),
            last: Some(pos),
            travelled: 0.0,
            carries: Vec::new(),
            symmetry: self.workspace.symmetry,
            copies: self.workspace.symmetry.copies(),
        });
        self.blend_mirrored(pos, pressure);
    }

    pub(crate) fn blend_drag(&mut self, pos: Vec2, pressure: f32) {
        let diameter = self.blend_diameter(pressure);
        let spacing = (diameter * self.brush_state.brush.brush_options.spacing / 100.0).max(1.0);
        let Some(stroke) = self.brush_state.blend_stroke.as_mut() else {
            return;
        };
        let Some(last) = stroke.last else {
            return;
        };
        let travel = pos - last;
        let length = travel.length();
        if length <= 0.0 {
            return;
        }
        let dir = travel / length;
        let mut t = spacing - stroke.travelled;
        let mut dabs = Vec::new();
        while t <= length {
            dabs.push(last + dir * t);
            t += spacing;
        }
        stroke.travelled = length - (t - spacing);
        stroke.last = Some(pos);
        for p in dabs {
            self.blend_mirrored(p, pressure);
        }
    }

    pub(crate) fn blend_release(&mut self) {
        let Some(stroke) = self.brush_state.blend_stroke.take() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(stroke.layer_id) else {
            return;
        };
        if stroke.before.is_empty() {
            return;
        }
        let ts = self.canvas.tile_size();
        let tiles = stroke
            .before
            .into_iter()
            .map(|((tx, ty), data)| TileSnapshot {
                tx,
                ty,
                layer_id: stroke.layer_id,
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: data.into(),
            })
            .collect();
        if let Some(history) = self.layer_state.histories.get_mut(idx) {
            history.push_action(UndoAction {
                tiles,
                selection: None,
                transform: None,
                layer_action: None,
            });
        }
        self.layer_state.thumbnails_dirty = true;
    }

    fn blend_diameter(&self, pressure: f32) -> f32 {
        let o = &self.brush_state.brush.brush_options;
        let k = if o.pressure_size {
            o.pressure_min_size + (1.0 - o.pressure_min_size) * pressure
        } else {
            1.0
        };
        (o.diameter * k).max(1.0)
    }

    /// A dab at `center` and its mirror copies.
    fn blend_mirrored(&mut self, center: Vec2, pressure: f32) {
        let Some(stroke) = self.brush_state.blend_stroke.as_ref() else {
            return;
        };
        let positions = stroke.symmetry.positions(&stroke.copies, center);
        for (copy, p) in positions {
            self.blend_dab(p, pressure, copy);
        }
    }

    /// One dab at `center`, for mirror copy `copy`.
    fn blend_dab(&mut self, center: Vec2, pressure: f32, copy: usize) {
        let diameter = self.blend_diameter(pressure);
        let o = &self.brush_state.brush.brush_options;
        let r = diameter * 0.5;
        let mut strength = o.flow / 100.0 * o.opacity;
        if o.pressure_opacity {
            strength *= pressure;
        }
        if o.pressure_flow {
            strength *= pressure;
        }
        let hardness = (o.hardness / 100.0).clamp(0.0, 1.0);
        let (length, blur_size) = (
            self.workspace.blend.smudge_length.clamp(0.0, 1.0),
            self.workspace.blend.blur_size,
        );
        let Some(stroke) = self.brush_state.blend_stroke.as_mut() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(stroke.layer_id) else {
            return;
        };
        let alpha_lock = self.canvas.layers[idx].alpha_locked;
        let rc = r.ceil() as i32;
        let side = (2 * rc + 1) as usize;
        let (x0, y0) = (center.x.floor() as i32 - rc, center.y.floor() as i32 - rc);

        // The brush tip, shaped like the brush's (hardness falloff), times
        // the stroke strength and the selection.
        let mut mask = vec![0.0f32; side * side];
        let mut row_sel = vec![1.0f32; side];
        for ly in 0..side {
            let y = y0 + ly as i32;
            if self.selection_manager.has_selection() {
                if y < 0 {
                    continue;
                }
                let start = x0.max(0);
                row_sel.fill(0.0);
                let skip = (start - x0) as usize;
                if skip < side {
                    self.selection_manager.row_coverage(
                        y as usize,
                        start as usize,
                        &mut row_sel[skip..],
                    );
                }
            }
            for lx in 0..side {
                let p = Vec2::new((x0 + lx as i32) as f32 + 0.5, y as f32 + 0.5);
                let t = (p - center).length() / r.max(0.5);
                if t < 1.0 {
                    mask[ly * side + lx] =
                        crate::brush_engine::masks::gaussian_falloff(t, hardness)
                            * strength
                            * row_sel[lx];
                }
            }
        }

        let under: Vec<[f32; 4]> = self
            .canvas
            .render_reference(Some(idx), x0, y0, side, side)
            .into_iter()
            .map(to_f)
            .collect();
        let target: Vec<[f32; 4]> = if stroke.smudge {
            // The carried paint, resized if pressure changed the tip size.
            if stroke.carries.len() <= copy {
                stroke.carries.resize_with(copy + 1, || None);
            }
            let carry = match stroke.carries[copy].take() {
                Some(c) if c.side == side => c,
                Some(c) => Carry {
                    side,
                    px: (0..side * side)
                        .map(|i| {
                            let (x, y) = (i % side * c.side / side, i / side * c.side / side);
                            c.px[y * c.side + x]
                        })
                        .collect(),
                },
                None => Carry {
                    side,
                    px: under.clone(),
                },
            };
            let px = carry.px.clone();
            stroke.carries[copy] = Some(carry);
            px
        } else {
            let radius = ((r * blur_size).round() as usize).max(1);
            box_blur(&under, side, radius)
        };

        let mut result = Vec::with_capacity(side * side);
        let mut changed = false;
        for i in 0..side * side {
            let (u, t, m) = (under[i], target[i], mask[i]);
            let mut v = [0.0; 4];
            for c in 0..4 {
                v[c] = u[c] + (t[c] - u[c]) * m;
            }
            let mut out = to_c(v);
            let before = to_c(u);
            if alpha_lock {
                out = crate::canvas::blend::with_alpha_of(out, before.a());
            }
            changed |= out != before;
            result.push(out);
        }
        // Smudge picks up the blended paint for the next dab.
        if let Some(carry) = stroke.carries.get_mut(copy).and_then(|c| c.as_mut()) {
            for (c, &res) in carry.px.iter_mut().zip(&result) {
                let res = to_f(res);
                for k in 0..4 {
                    c[k] = res[k] + (c[k] - res[k]) * length;
                }
            }
        }
        if !changed {
            return;
        }
        self.canvas.write_layer_region(
            idx,
            (x0, y0, side, side),
            &result,
            Some(&mut stroke.before),
        );
        self.mark_rect_damage([x0, y0, x0 + side as i32, y0 + side as i32]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_blur_keeps_flat_areas_and_softens_edges() {
        let side = 9;
        let flat = vec![[10.0, 20.0, 30.0, 255.0]; side * side];
        let out = box_blur(&flat, side, 2);
        assert!(
            out.iter()
                .all(|p| (p[0] - 10.0).abs() < 1e-3 && (p[3] - 255.0).abs() < 1e-3)
        );
        // A hard vertical edge becomes a ramp.
        let edge: Vec<[f32; 4]> = (0..side * side)
            .map(|i| if i % side < 4 { [0.0; 4] } else { [1.0; 4] })
            .collect();
        let out = box_blur(&edge, side, 2);
        let mid = out[4 * side + 4][0];
        assert!(mid > 0.08 && mid < 0.92, "{mid}");
    }
}
