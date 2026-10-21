//! Vector and text layers sharp when zoomed in. The canvas is shown from
//! its composited pixels, which zooming in only enlarges; so past
//! [`SHARP_FROM`] screen pixels per canvas pixel, the vector and text
//! layers at the top of the stack are drawn again from their lines and
//! glyphs at the screen's resolution (in the background), and left out of
//! the composite while that picture shows over it.
//!
//! Only layers at the top qualify (nothing but such layers above them):
//! their picture goes over the rest as they would. A layer with a blend
//! mode, a mask, clipping, an effect or a pose, one being typed into, or
//! any of them under a layer that doesn't qualify, shows its pixels as
//! before. Until a picture for the view and the layers as they are is
//! ready, the last one shows where it was made (scaled), as the composite
//! did.

use crate::PainterApp;
use crate::app::view::render::{CanvasView, ScreenMap};
use crate::canvas::storage::LayerId;
use crate::canvas::text::TextLayer;
use crate::canvas::vector::{self, VectorItem};
use eframe::egui::{self, Color32, Vec2};
use std::sync::{Arc, mpsc};

/// Screen pixels per canvas pixel past which the layers are drawn sharp.
pub const SHARP_FROM: f32 = 1.5;
/// Largest side of the picture, pixels.
const MAX_SIDE: usize = 8192;

/// One layer as the picture draws it.
#[derive(Clone)]
enum Source {
    Lines(Vec<Arc<VectorItem>>),
    Text(Box<TextLayer>, Vec<ab_glyph::FontArc>),
}

/// What a picture is drawn from: the layers (bottom first, with their
/// opacity), the canvas point at its top-left, its size and pixels per
/// canvas pixel.
struct Request {
    layers: Vec<(Source, f32)>,
    rect: [f32; 2],
    size: [usize; 2],
    scale: f32,
}

/// What a picture shows: the view and layers (`view`), and their lines
/// (`content`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Keys {
    view: u64,
    content: u64,
}

/// A picture drawn.
struct Picture {
    keys: Keys,
    ids: Vec<LayerId>,
    origin: Vec2,
    size: [usize; 2],
    scale: f32,
    pixels: Vec<Color32>,
}

/// The picture showing, and where.
struct Shown {
    texture: egui::TextureHandle,
    origin: Vec2,
    /// Its size in canvas pixels.
    extent: Vec2,
    scale: f32,
    keys: Keys,
}

#[derive(Default)]
pub struct SharpState {
    /// Layers left out of the composite (their picture shows instead).
    pub(crate) hidden: Vec<LayerId>,
    shown: Option<Shown>,
    running: Option<mpsc::Receiver<Picture>>,
    /// What the last picture asked for was drawn from.
    asked: Option<Keys>,
    /// Where the lines changed since (canvas pixels): drawn again in place
    /// when that's all that changed and it's small.
    damage: Option<[i32; 4]>,
}

/// Largest change (picture pixels) drawn again in place, at once.
const PATCH_PIXELS: usize = 512 * 512;

/// Whether layer `i` can be drawn sharp: a plain vector or text layer.
fn qualifies(app: &PainterApp, i: usize) -> bool {
    let canvas = &app.canvas;
    let l = &canvas.layers[i];
    let typing = app
        .workspace
        .text
        .session
        .as_ref()
        .is_some_and(|s| canvas.layer_index_of(s.layer_id()) == Some(i));
    (l.vector.is_some() || l.text.is_some())
        && !typing
        && l.parent.is_none()
        && matches!(l.kind, crate::canvas::storage::LayerKind::Paint)
        && l.blend == crate::canvas::blend_modes::LayerBlend::Normal
        && !l.clipped
        && l.adjustment.is_none()
        && l.style.is_plain()
        && l.shader.is_none()
        && l.height.is_none()
        && l.wet.is_none()
        && l.anim.is_none()
        && l.motion.is_none()
        && l.rig.is_none()
        && l.warps.is_empty()
        && !l.is_posed()
        && !canvas.layers.iter().any(
            |m| matches!(m.kind, crate::canvas::storage::LayerKind::Mask { owner } if owner == l.id),
        )
}

/// The layers to draw sharp: the run of qualifying layers at the top
/// (invisible layers skipped), bottom first.
fn top_run(app: &PainterApp) -> Vec<usize> {
    let mut run = Vec::new();
    for i in (0..app.canvas.layers.len()).rev() {
        let l = &app.canvas.layers[i];
        // (A folder's children are under it in the list: only top-level
        // layers are looked at, and a folder stops the run.)
        if !l.visible
            && l.parent.is_none()
            && !matches!(l.kind, crate::canvas::storage::LayerKind::Group)
        {
            continue;
        }
        if !qualifies(app, i) {
            break;
        }
        run.push(i);
    }
    run.reverse();
    run
}

/// Whether anything going on draws the canvas some other way (a live
/// preview, a transform, wrap-around): then nothing is drawn sharp.
fn busy(app: &PainterApp) -> bool {
    app.workspace.wrap_around
        || app.render_cache.live.is_some()
        || app.layer_state.floating_layer_idx.is_some()
        || app.layer_state.in_place.is_some()
        || app.layer_state.liquify.is_some()
        || app.layer_state.puppet.is_some()
        || app.workspace.filter.live()
        || app.quick_mask_layer().is_some()
}

impl PainterApp {
    /// Once a frame, before the composite: decide what's drawn sharp, put
    /// in a finished picture, and ask for a new one if the view or the
    /// layers changed.
    pub(crate) fn sharp_tick(
        &mut self,
        ctx: &egui::Context,
        view: &CanvasView,
        clip: egui::Rect,
    ) -> bool {
        let ppp = ctx.pixels_per_point();
        let scale = self.viewport.zoom * ppp;
        let run = if scale >= SHARP_FROM && !busy(self) {
            top_run(self)
        } else {
            Vec::new()
        };
        let ids: Vec<LayerId> = run.iter().map(|&i| self.canvas.layers[i].id).collect();
        if ids.is_empty() {
            self.sharp_off();
            return false;
        }
        // Layers no longer drawn sharp show in the composite again at once.
        let state = &mut self.workspace.vector.sharp;
        if !state.hidden.is_empty() && state.hidden != ids {
            self.sharp_off();
        }
        let map = crate::app::view::render::screen_map(self, view);
        let Some((origin, extent)) = self.sharp_area(&map, clip.intersect(view.response.rect), ppp)
        else {
            return false;
        };
        // A huge screen: no more than MAX_SIDE pixels a side.
        let scale = scale.min(MAX_SIDE as f32 / extent.x.max(extent.y));
        let size = [
            ((extent.x * scale).ceil() as usize).clamp(1, MAX_SIDE),
            ((extent.y * scale).ceil() as usize).clamp(1, MAX_SIDE),
        ];
        let keys = self.sharp_keys(&run, origin, size, scale);
        let mut busy = false;
        // A picture done: shown, and its layers left out of the composite.
        let state = &mut self.workspace.vector.sharp;
        if let Some(rx) = &state.running {
            match rx.try_recv() {
                Ok(picture) => {
                    state.running = None;
                    if picture.ids == ids {
                        let image = egui::ColorImage {
                            size: picture.size,
                            pixels: self.sharp_display(picture.pixels),
                        };
                        let state = &mut self.workspace.vector.sharp;
                        let options = egui::TextureOptions::LINEAR;
                        match &mut state.shown {
                            Some(s) if s.texture.size() == picture.size => {
                                s.texture.set(image, options)
                            }
                            _ => {
                                state.shown = Some(Shown {
                                    texture: ctx.load_texture("sharp-vectors", image, options),
                                    origin: picture.origin,
                                    extent: Vec2::ZERO,
                                    scale: picture.scale,
                                    keys: picture.keys,
                                })
                            }
                        }
                        if let Some(s) = &mut state.shown {
                            s.origin = picture.origin;
                            s.extent = Vec2::new(picture.size[0] as f32, picture.size[1] as f32)
                                / picture.scale;
                            s.scale = picture.scale;
                            s.keys = picture.keys;
                        }
                        if state.hidden != picture.ids {
                            state.hidden = picture.ids;
                            self.mark_all_tiles_dirty();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => busy = true,
                Err(mpsc::TryRecvError::Disconnected) => state.running = None,
            }
        }
        // Only some lines changed, in a small area: that part drawn again
        // now, in place.
        if self.sharp_patch(&run, keys) {
            return busy;
        }
        let state = &mut self.workspace.vector.sharp;
        if state.running.is_none() && state.asked != Some(keys) {
            state.asked = Some(keys);
            state.damage = None;
            let reach = [
                origin.x.floor() as i32 - 1,
                origin.y.floor() as i32 - 1,
                (origin.x + size[0] as f32 / scale).ceil() as i32 + 1,
                (origin.y + size[1] as f32 / scale).ceil() as i32 + 1,
            ];
            let request = self.sharp_request(&run, origin, size, scale, reach);
            let (tx, rx) = mpsc::channel();
            let ids = ids.clone();
            self.workspace.pool.spawn(move || {
                let pixels = draw(&request);
                let _ = tx.send(Picture {
                    keys,
                    ids,
                    origin: Vec2::new(request.rect[0], request.rect[1]),
                    size: request.size,
                    scale: request.scale,
                    pixels,
                });
            });
            self.workspace.vector.sharp.running = Some(rx);
            busy = true;
        }
        busy
    }

    /// Nothing drawn sharp: every layer back in the composite.
    fn sharp_off(&mut self) {
        let state = &mut self.workspace.vector.sharp;
        state.shown = None;
        state.asked = None;
        if !state.hidden.is_empty() {
            state.hidden.clear();
            self.mark_all_tiles_dirty();
        }
    }

    /// The canvas rectangle the screen shows in `clip` (its top-left on a
    /// screen pixel's corner when the view isn't turned): top-left and size.
    fn sharp_area(&self, map: &ScreenMap, clip: egui::Rect, ppp: f32) -> Option<(Vec2, Vec2)> {
        if !clip.is_positive() {
            return None;
        }
        let corners = [
            clip.left_top(),
            clip.right_top(),
            clip.right_bottom(),
            clip.left_bottom(),
        ]
        .map(|p| map.to_canvas(p));
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        let lo = corners
            .iter()
            .fold(Vec2::splat(f32::MAX), |a, c| a.min(*c))
            .max(Vec2::ZERO);
        let hi = corners
            .iter()
            .fold(Vec2::splat(f32::MIN), |a, c| a.max(*c))
            .min(Vec2::new(w, h));
        if lo.x >= hi.x || lo.y >= hi.y {
            return None;
        }
        // Texels on screen pixels: the top-left where a pixel's edge is.
        let plain = self.viewport.rotation == 0.0 && !self.viewport.flip_x;
        let lo = if plain {
            let at = map.to_screen(lo) * ppp;
            let snapped = egui::pos2(at.x.floor(), at.y.floor()) / ppp;
            map.to_canvas(snapped)
        } else {
            lo
        };
        Some((lo, hi - lo))
    }

    /// What the picture for `run` over this area would be drawn from.
    fn sharp_keys(&self, run: &[usize], origin: Vec2, size: [usize; 2], scale: f32) -> Keys {
        use std::hash::{Hash, Hasher};
        let (mut view, mut content) = (
            rustc_hash::FxHasher::default(),
            rustc_hash::FxHasher::default(),
        );
        for &i in run {
            let l = &self.canvas.layers[i];
            l.id.0.hash(&mut view);
            l.opacity.to_bits().hash(&mut view);
            if let Some(v) = &l.vector {
                v.version().hash(&mut content);
            }
            if let Some(t) = &l.text {
                format!("{t:?}").hash(&mut view);
            }
        }
        (
            origin.x.to_bits(),
            origin.y.to_bits(),
            size,
            scale.to_bits(),
        )
            .hash(&mut view);
        Keys {
            view: view.finish(),
            content: content.finish(),
        }
    }

    /// Lines of a vector layer changed over `region` (canvas pixels).
    pub(crate) fn sharp_damage(&mut self, region: [i32; 4]) {
        let state = &mut self.workspace.vector.sharp;
        if state.shown.is_some() {
            state.damage = vector::union(state.damage.into_iter().chain([region]));
        }
    }

    /// Draw the damaged part of the picture again in place, when the view
    /// and layers are as it shows them and only lines changed there.
    /// Returns whether it did.
    fn sharp_patch(&mut self, run: &[usize], keys: Keys) -> bool {
        let state = &self.workspace.vector.sharp;
        let Some(shown) = &state.shown else {
            return false;
        };
        let Some(damage) = state.damage else {
            return false;
        };
        if state.running.is_some()
            || shown.keys.view != keys.view
            || shown.keys.content == keys.content
        {
            return false;
        }
        let [tw, th] = shown.texture.size();
        let (origin, scale) = (shown.origin, shown.scale);
        let px = |v: i32, o: f32, n: usize, up: bool| {
            let p = (v as f32 - o) * scale;
            (if up { p.ceil() } else { p.floor() }).clamp(0.0, n as f32) as usize
        };
        let (x0, y0) = (
            px(damage[0] - 1, origin.x, tw, false),
            px(damage[1] - 1, origin.y, th, false),
        );
        let (x1, y1) = (
            px(damage[2] + 1, origin.x, tw, true),
            px(damage[3] + 1, origin.y, th, true),
        );
        let state = &mut self.workspace.vector.sharp;
        if x0 >= x1 || y0 >= y1 {
            // Off screen: nothing to draw.
            state.damage = None;
            if let Some(s) = &mut state.shown {
                s.keys = keys;
            }
            state.asked = Some(keys);
            return true;
        }
        if (x1 - x0) * (y1 - y0) > PATCH_PIXELS {
            return false;
        }
        // The patch as a picture of its own.
        let at = origin + Vec2::new(x0 as f32, y0 as f32) / scale;
        let reach = [damage[0] - 2, damage[1] - 2, damage[2] + 2, damage[3] + 2];
        let request = self.sharp_request(run, at, [x1 - x0, y1 - y0], scale, reach);
        let pixels = self.sharp_display(draw(&request));
        let image = egui::ColorImage {
            size: [x1 - x0, y1 - y0],
            pixels,
        };
        let state = &mut self.workspace.vector.sharp;
        if let Some(s) = &mut state.shown {
            s.texture
                .set_partial([x0, y0], image, egui::TextureOptions::LINEAR);
            s.keys = keys;
        }
        state.asked = Some(keys);
        state.damage = None;
        true
    }

    fn sharp_request(
        &mut self,
        run: &[usize],
        origin: Vec2,
        size: [usize; 2],
        scale: f32,
        reach: [i32; 4],
    ) -> Request {
        let rect = [origin.x, origin.y];
        let mut layers = Vec::new();
        for &i in run {
            let l = &self.canvas.layers[i];
            let opacity = l.opacity;
            if let Some(v) = &l.vector {
                let near = v.candidates(reach);
                let items = near.into_iter().map(|k| v.items()[k].clone()).collect();
                layers.push((Source::Lines(items), opacity));
            } else if let Some(t) = l.text.clone()
                && let Some(fonts) = self.workspace.text.fonts_of(&t)
            {
                layers.push((Source::Text(t, fonts), opacity));
            }
        }
        Request {
            layers,
            rect,
            size,
            scale,
        }
    }

    /// The top vector and text layers drawn as a `size` picture from canvas
    /// point `origin` at `scale` (what a zoomed-in screen asks for), or a
    /// patch of it, without a frame: for the benchmarks.
    #[cfg(feature = "bench")]
    pub(crate) fn sharp_picture(
        &mut self,
        origin: Vec2,
        size: [usize; 2],
        scale: f32,
    ) -> Vec<Color32> {
        let run = top_run(self);
        let reach = [
            origin.x.floor() as i32 - 2,
            origin.y.floor() as i32 - 2,
            (origin.x + size[0] as f32 / scale).ceil() as i32 + 2,
            (origin.y + size[1] as f32 / scale).ceil() as i32 + 2,
        ];
        let request = self.sharp_request(&run, origin, size, scale, reach);
        self.sharp_display(draw(&request))
    }

    /// Pixels as the monitor shows them (the composite's colour management).
    fn sharp_display(&self, pixels: Vec<Color32>) -> Vec<Color32> {
        let Some(display) = &self.render_cache.display else {
            return pixels;
        };
        let mut bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_array()).collect();
        display.apply(&mut bytes);
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, a]| Color32::from_rgba_premultiplied(r, g, b, a))
            .collect()
    }
}

/// The picture: each layer drawn at the request's scale and laid over the
/// ones below.
fn draw(r: &Request) -> Vec<Color32> {
    let [w, h] = r.size;
    let origin = Vec2::new(r.rect[0], r.rect[1]);
    let s = r.scale;
    let to_picture = move |p: Vec2| (p - origin) * s;
    let mut out = vec![Color32::TRANSPARENT; w * h];
    let region = [0, 0, w as i32, h as i32];
    for (source, opacity) in &r.layers {
        let pixels: Vec<Color32> = match source {
            Source::Lines(items) => {
                let mapped: Vec<VectorItem> = items
                    .iter()
                    .map(|i| {
                        let mut i = VectorItem::clone(i);
                        i.map_points_for_view(&to_picture, s);
                        i
                    })
                    .collect();
                vector::render_region(mapped.iter(), region)
            }
            Source::Text(t, fonts) => {
                let place = [
                    s as f64,
                    0.0,
                    (-origin.x * s) as f64,
                    0.0,
                    s as f64,
                    (-origin.y * s) as f64,
                    0.0,
                    0.0,
                    1.0,
                ];
                let m = match &t.transform {
                    Some(m) => crate::canvas::storage::mul3(&place, m),
                    None => place,
                };
                let mut px = vec![Color32::TRANSPARENT; w * h];
                if let Some(mask) =
                    crate::canvas::text::render(fonts, &t.text, &t.style, t.pos, Some(&m))
                {
                    let [cr, cg, cb, ca] = t.color.to_srgba_unmultiplied();
                    for my in 0..mask.h {
                        let y = mask.y0 + my as i32;
                        if y < 0 || y as usize >= h {
                            continue;
                        }
                        for mx in 0..mask.w {
                            let x = mask.x0 + mx as i32;
                            let cov = mask.data[my * mask.w + mx];
                            if cov == 0 || x < 0 || x as usize >= w {
                                continue;
                            }
                            let a = (ca as u32 * cov as u32 + 127) / 255;
                            px[y as usize * w + x as usize] =
                                Color32::from_rgba_unmultiplied(cr, cg, cb, a as u8);
                        }
                    }
                }
                px
            }
        };
        let k = opacity.clamp(0.0, 1.0);
        for (d, src) in out.iter_mut().zip(pixels) {
            if src.a() == 0 {
                continue;
            }
            let src = if k < 1.0 { src.gamma_multiply(k) } else { src };
            let inv = 255 - src.a() as u32;
            let mix = |a: u8, b: u8| (a as u32 + (b as u32 * inv + 127) / 255).min(255) as u8;
            *d = Color32::from_rgba_premultiplied(
                mix(src.r(), d.r()),
                mix(src.g(), d.g()),
                mix(src.b(), d.b()),
                mix(src.a(), d.a()),
            );
        }
    }
    out
}

/// Draw the picture over the canvas, where it was made.
pub(crate) fn draw_sharp(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap) {
    let state = &app.workspace.vector.sharp;
    let Some(shown) = &state.shown else {
        return;
    };
    if state.hidden.is_empty() {
        return;
    }
    let (o, e) = (shown.origin, shown.extent);
    let corners =
        [o, o + Vec2::new(e.x, 0.0), o + e, o + Vec2::new(0.0, e.y)].map(|p| map.to_screen(p));
    let uvs = [
        egui::pos2(0.0, 0.0),
        egui::pos2(1.0, 0.0),
        egui::pos2(1.0, 1.0),
        egui::pos2(0.0, 1.0),
    ];
    let mut mesh = egui::Mesh::with_texture(shown.texture.id());
    for (p, uv) in corners.into_iter().zip(uvs) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: p,
            uv,
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    fn app() -> PainterApp {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(200, 150, Color32::WHITE, 64));
        app.selection_manager.canvas_size = [200, 150];
        app.canvas_mut().active_layer_idx = 1;
        app.add_vector_layer();
        let b = &mut app.brush_state.brush.brush_options;
        b.color = Color32::BLACK;
        b.diameter = 6.0;
        b.pressure_size = false;
        app.start_stroke_with_pressure(Vec2::new(20.0, 40.0), 1.0);
        for i in 1..=20 {
            app.add_stroke_point(Vec2::new(20.0 + i as f32 * 8.0, 40.0), 1.0);
        }
        app.finish_stroke();
        app.settle_vectors();
        app
    }

    /// Frames until `done` (or two seconds).
    fn frames(app: &mut PainterApp, ctx: &egui::Context, done: impl Fn(&PainterApp) -> bool) {
        let start = std::time::Instant::now();
        while start.elapsed().as_secs_f32() < 2.0 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let view = crate::app::view::render::draw_canvas(app, ui);
                    app.sharp_tick(ctx, &view, ui.clip_rect());
                });
            });
            if done(app) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn zoomed_in_the_top_vector_layer_is_drawn_sharp_and_left_out_of_the_composite() {
        let mut app = app();
        let ctx = egui::Context::default();
        let id = app.canvas.layers[app.canvas.active_layer_idx].id;
        app.viewport.zoom = 4.0;
        frames(&mut app, &ctx, |a| {
            !a.workspace.vector.sharp.hidden.is_empty()
        });
        assert_eq!(app.workspace.vector.sharp.hidden, vec![id]);
        assert!(app.workspace.vector.sharp.shown.is_some());
        // Let any picture still being drawn come in.
        frames(&mut app, &ctx, |a| {
            a.workspace.vector.sharp.running.is_none()
        });
        // A short new line: drawn into the picture in place, that frame.
        app.start_stroke_with_pressure(Vec2::new(30.0, 60.0), 1.0);
        app.add_stroke_point(Vec2::new(40.0, 62.0), 1.0);
        app.finish_stroke();
        frames(&mut app, &ctx, |_| true);
        let state = &app.workspace.vector.sharp;
        assert!(state.running.is_none(), "no picture of the whole screen");
        assert_eq!(state.shown.as_ref().map(|s| s.keys), state.asked);
        assert!(state.damage.is_none());
        // A paint layer on top: the vector layer isn't at the top any more.
        app.add_layer_and_select();
        frames(&mut app, &ctx, |a| {
            a.workspace.vector.sharp.hidden.is_empty()
        });
        assert!(app.workspace.vector.sharp.hidden.is_empty());
        app.apply_history(false);
        frames(&mut app, &ctx, |a| {
            !a.workspace.vector.sharp.hidden.is_empty()
        });
        // Zoomed out: back in the composite.
        app.viewport.zoom = 1.0;
        frames(&mut app, &ctx, |a| {
            a.workspace.vector.sharp.hidden.is_empty()
        });
        assert!(app.workspace.vector.sharp.hidden.is_empty());
        assert!(app.workspace.vector.sharp.shown.is_none());
    }

    #[test]
    fn the_picture_has_edges_a_screen_pixel_wide() {
        let app = app();
        let v = app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap();
        let request = Request {
            layers: vec![(Source::Lines(v.items().to_vec()), 1.0)],
            rect: [0.0, 30.0],
            size: [400, 80],
            scale: 4.0,
        };
        let pixels = draw(&request);
        // Down a column across the line (6 px wide at y 40: rows 16..64
        // here): fully covered inside, clear outside, and at most two rows
        // part covered at each edge.
        let column: Vec<u8> = (0..80).map(|y| pixels[y * 400 + 200].a()).collect();
        assert_eq!(column[40], 255);
        assert_eq!(column[2], 0);
        let partial = column.iter().filter(|&&a| a > 0 && a < 255).count();
        assert!(partial <= 4, "{column:?}");
    }
}
