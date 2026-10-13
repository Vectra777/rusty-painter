/// Drives the real screen-update path (`update_dirty_textures`) over a
/// few frames and applies its uploads to an in-memory atlas, then checks
/// the atlas shows exactly the layers' composite.
struct ScreenSim {
    ctx: egui::Context,
    atlas: std::collections::HashMap<usize, Vec<[u8; 4]>>,
    /// The pointer is down (a slider being dragged).
    dragging: bool,
    /// Frames to settle in before giving up.
    max_frames: usize,
    /// Run `max_frames` and stop, settled or not (off-screen tiles stay
    /// dirty when zoomed in).
    unsettled_ok: bool,
}

impl ScreenSim {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            atlas: Default::default(),
            dragging: false,
            max_frames: 50,
            unsettled_ok: false,
        }
    }

    /// Run frames until no dirty tiles are left.
    fn settle(&mut self, app: &mut PainterApp) {
        self.run_frames(app, true);
    }

    /// Run frames until no dirty tiles are left, without keeping the
    /// uploads; returns how many it took and the time spent updating.
    fn settle_counting(&mut self, app: &mut PainterApp) -> (usize, std::time::Duration) {
        self.run_frames(app, false)
    }

    fn run_frames(&mut self, app: &mut PainterApp, keep: bool) -> (usize, std::time::Duration) {
        use crate::app::view::gpu_canvas::ATLAS_TEXTURE_SIZE;
        let mut spent = std::time::Duration::ZERO;
        for frame in 1..=self.max_frames {
            let mut more = false;
            let mut uploads = Vec::new();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 900.0),
                )),
                ..Default::default()
            };
            let _ = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let view = draw_canvas(app, ui);
                    let started = std::time::Instant::now();
                    // As the frame does: a filter's run, then the screen.
                    let screen = preview_view(app, &view, view.rect);
                    app.filter_update(self.dragging.then_some(&screen));
                    app.liquify_frame(&screen);
                    let (u, m) = update_dirty_textures(app, &view, view.rect);
                    spent += started.elapsed();
                    uploads = u;
                    more = m;
                });
            });
            if !keep {
                uploads.clear();
            }
            for up in uploads {
                if up.level > 0 {
                    // (Only level 0 is simulated.)
                    assert!(self.dragging, "not drawing: full resolution");
                    continue;
                }
                let tex = self
                    .atlas
                    .entry(up.atlas)
                    .or_insert_with(|| vec![[0; 4]; ATLAS_TEXTURE_SIZE * ATLAS_TEXTURE_SIZE]);
                for row in 0..up.height as usize {
                    for col in 0..up.width as usize {
                        let s = (row * up.width as usize + col) * 4;
                        let d = (up.y as usize + row) * ATLAS_TEXTURE_SIZE + up.x as usize + col;
                        tex[d].copy_from_slice(&up.pixels[s..s + 4]);
                    }
                }
            }
            if !more && !app.render_cache.tiles.iter().any(|t| t.dirty) {
                return (frame, spent);
            }
        }
        if self.unsettled_ok {
            return (self.max_frames, spent);
        }
        panic!("screen never settled");
    }

    /// Canvas pixels whose on-screen texel differs from the composite.
    fn mismatches(&self, app: &PainterApp) -> Vec<(usize, usize)> {
        use crate::app::view::gpu_canvas::{ATLAS_BORDER, ATLAS_TEXTURE_SIZE};
        let img = app.canvas.flatten();
        let (w, h) = (app.canvas.width(), app.canvas.height());
        let mut bad = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let (atlas, lx, ly) = app.render_cache.atlas_slot(x / TILE_SIZE, y / TILE_SIZE);
                let texel = (ATLAS_BORDER + ly + y % TILE_SIZE) * ATLAS_TEXTURE_SIZE
                    + ATLAS_BORDER
                    + lx
                    + x % TILE_SIZE;
                let shown = self.atlas.get(&atlas).map_or([0; 4], |t| t[texel]);
                if shown != img.pixels[y * w + x].to_array() {
                    bad.push((x, y));
                }
            }
        }
        bad
    }
}

/// The paint layer's pixels, tile by tile.
fn layer_pixels(app: &PainterApp, idx: usize) -> Vec<Vec<Color32>> {
    let mut keys = app.canvas.layer_tile_keys(idx);
    keys.sort_unstable();
    keys.into_iter()
        .filter_map(|(tx, ty)| app.canvas.get_layer_tile_data(idx, tx, ty))
        .collect()
}

#[test]
fn a_dragged_filter_previews_on_screen_and_ends_exactly_as_before() {
    use crate::canvas::filters::Filter;
    use crate::selection::{SelectionMode, SelectionShape};
    for filter in [
        Filter::HueSaturation {
            hue: 70.0,
            saturation: 0.3,
            lightness: 0.1,
        },
        Filter::GaussianBlur { radius: 6.0 },
    ] {
        let setup = || {
            let mut app = painted(512);
            app.viewport.zoom = 0.25;
            app.viewport.offset = eframe::egui::Vec2::ZERO;
            // A soft selection: only part changes, its edge blends.
            app.selection_manager.apply_shape(
                SelectionShape::Circle {
                    center: eframe::egui::Vec2::new(250.0, 240.0),
                    radius: 180.0,
                },
                SelectionMode::Replace,
            );
            app
        };
        // The result as a filter always gave it: run once, then kept.
        let mut direct = setup();
        direct.filter_open(filter);
        direct.filter_commit();
        let expected = layer_pixels(&direct, 1);

        let mut app = setup();
        let mut screen = ScreenSim::new();
        screen.settle(&mut app);
        let before = layer_pixels(&app, 1);
        app.filter_open(Filter::HueSaturation {
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
        });
        screen.settle(&mut app);
        app.workspace.filter.session.as_mut().unwrap().set_slow();
        screen.dragging = true;
        for step in [0.2_f32, 0.6, 1.0] {
            // Part way there, then the value itself.
            let session = app.workspace.filter.session.as_mut().unwrap();
            session.filter = if step < 1.0 {
                Filter::GaussianBlur { radius: step }
            } else {
                filter
            };
            session.dirty = true;
            let (frames, _) = screen.settle_counting(&mut app);
            assert_eq!(frames, 1, "{filter:?}: all on screen in one frame");
            assert!(app.workspace.filter.live());
        }
        assert!(
            layer_pixels(&app, 1) == before,
            "{filter:?}: the preview leaves the layer as it was"
        );
        screen.dragging = false;
        screen.settle(&mut app);
        assert!(!app.workspace.filter.live());
        assert!(
            screen.mismatches(&app).is_empty(),
            "{filter:?}: let go, the screen shows the layer exactly"
        );
        app.filter_commit();
        assert!(
            layer_pixels(&app, 1) == expected,
            "{filter:?}: kept as before"
        );
        assert_eq!(app.layer_state.history.stacks().0.len(), 1, "one step");
        app.apply_history(false);
        assert!(layer_pixels(&app, 1) == before, "{filter:?}: undone");
    }
}

#[test]
fn a_dragged_adjustment_previews_then_shows_exactly() {
    use crate::canvas::filters::Filter;
    let mut app = painted(512);
    app.viewport.zoom = 0.25;
    app.viewport.offset = eframe::egui::Vec2::ZERO;
    let mut screen = ScreenSim::new();
    app.add_adjustment_layer(Filter::Levels {
        black: 0.0,
        white: 1.0,
        gamma: 1.0,
    });
    app.add_mask_to_active();
    screen.settle(&mut app);
    let idx = app
        .canvas
        .layer_index_of(app.workspace.filter.editing.unwrap());
    screen.dragging = true;
    for gamma in [1.5, 2.0, 3.0] {
        app.workspace.filter.adjusting = true;
        app.canvas_mut().layers[idx.unwrap()].adjustment = Some(Filter::Levels {
            black: 0.1,
            white: 0.9,
            gamma,
        });
        app.mark_all_tiles_dirty();
        let (frames, _) = screen.settle_counting(&mut app);
        assert_eq!(frames, 1, "all on screen in one frame");
    }
    screen.dragging = false;
    screen.settle(&mut app);
    assert!(!app.workspace.filter.adjusting);
    assert!(screen.mismatches(&app).is_empty(), "let go: exact");
}

#[test]
fn screen_matches_the_layers_after_liquify_undo_redo() {
    use crate::app::tools::Tool;
    use crate::canvas::liquify::LiquifyMode;
    let canvas = crate::canvas::Canvas::new(512, 384, Color32::WHITE, TILE_SIZE);
    let mut app = crate::project::tests::test_app_pub(canvas);
    app.recreate_render_cache(512, 384);
    app.canvas_mut().active_layer_idx = 1;
    app.viewport.zoom = 1.0;
    app.viewport.offset = eframe::egui::Vec2::ZERO;
    app.workspace.auto_fit = false;
    let mut screen = ScreenSim::new();
    screen.settle(&mut app);
    assert!(screen.mismatches(&app).is_empty(), "blank canvas");

    for (color, y) in [
        (Color32::from_rgb(210, 40, 40), 150.0),
        (Color32::from_rgb(210, 100, 40), 200.0),
    ] {
        app.brush_state.brush.brush_options.color = color;
        app.brush_state.brush.brush_options.diameter = 70.0;
        app.start_stroke_with_pressure(eframe::egui::Vec2::new(30.0, y), 1.0);
        for i in 1..=30 {
            app.add_stroke_point(eframe::egui::Vec2::new(30.0 + i as f32 * 14.0, y), 1.0);
            app.stroke_worker.wait_idle();
            app.sync_stroke_worker();
            screen.settle(&mut app);
        }
        app.finish_stroke();
        app.settle_strokes();
        screen.settle(&mut app);
    }
    assert!(screen.mismatches(&app).is_empty(), "after strokes");

    app.active_tool = Tool::Liquify;
    app.workspace.liquify.radius = 80.0;
    for (mode, y) in [
        (LiquifyMode::Push, 240.0),
        (LiquifyMode::TwirlCw, 220.0),
        (LiquifyMode::Bloat, 200.0),
    ] {
        app.workspace.liquify.mode = mode;
        app.liquify_press(eframe::egui::Vec2::new(150.0, y));
        screen.settle(&mut app);
        for i in 1..=12 {
            app.liquify_drag(eframe::egui::Vec2::new(
                150.0 + i as f32 * 9.0,
                y - i as f32 * 3.0,
            ));
            app.liquify_hold(1.0 / 60.0);
            screen.settle(&mut app);
        }
        app.liquify_release();
    }
    assert!(screen.mismatches(&app).is_empty(), "after liquify");
    for (step, redo) in [
        ("undo", false),
        ("redo", true),
        ("undo again", false),
        ("undo stroke", false),
    ] {
        app.apply_history(redo);
        screen.settle(&mut app);
        let bad = screen.mismatches(&app);
        assert!(
            bad.is_empty(),
            "{step}: {} stale pixels, e.g. {:?}",
            bad.len(),
            &bad[..bad.len().min(5)]
        );
    }
}

use super::*;
use crate::canvas::blend::downsample;

#[test]
fn with_a_live_shader_layer_each_run_fills_its_own_atlases() {
    use crate::app::view::gpu_canvas::{ATLAS_BORDER, ATLAS_TEXTURE_SIZE};
    use crate::canvas::shader::{self, LiveStatus};
    let mut app = painted(160);
    app.viewport.zoom = 1.0;
    app.viewport.offset = eframe::egui::Vec2::ZERO;
    let (name, source) = shader::TEMPLATES[0];
    app.add_shader_layer(name, source).unwrap();
    // A translucent paint layer above the shader layer.
    let above = app
        .add_layer_with_tiles("Above".into(), Vec::new(), |_| {})
        .unwrap();
    let dot: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
        .map(|i| {
            if (i % TILE_SIZE) < 20 {
                Color32::from_rgba_unmultiplied(200, 30, 30, 120)
            } else {
                Color32::TRANSPARENT
            }
        })
        .collect();
    app.canvas.set_layer_tile_data(above, 1, 1, dot);
    let LiveStatus::Live(layout) = shader::live_layout(&app.canvas) else {
        panic!("expected a live layout");
    };
    assert_eq!(layout.runs.len(), 2);
    app.render_cache.live = Some(std::sync::Arc::new(layout.clone()));
    app.mark_all_tiles_dirty();

    let mut screen = ScreenSim::new();
    screen.settle(&mut app);
    let per_run = app.render_cache.atlases_x * app.render_cache.atlases_y;
    let (w, h) = (app.canvas.width(), app.canvas.height());
    for (run, shown) in layout.runs.iter().enumerate() {
        let img = shader::run_view(&app.canvas, shown).flatten();
        for y in 0..h {
            for x in 0..w {
                let (atlas, lx, ly) = app.render_cache.atlas_slot(x / TILE_SIZE, y / TILE_SIZE);
                let texel = (ATLAS_BORDER + ly + y % TILE_SIZE) * ATLAS_TEXTURE_SIZE
                    + ATLAS_BORDER
                    + lx
                    + x % TILE_SIZE;
                let shown = screen
                    .atlas
                    .get(&(atlas + run * per_run))
                    .map_or([0; 4], |t| t[texel]);
                assert_eq!(
                    shown,
                    img.pixels[y * w + x].to_array(),
                    "run {run} at {x},{y}"
                );
            }
        }
    }
    // The run above holds only the translucent layer: the shader
    // layer's own (baked) pixels are in neither run.
    let top = shader::run_view(&app.canvas, &layout.runs[1]).flatten();
    assert_eq!(top.pixels[0], Color32::TRANSPARENT);
}

#[test]
fn atlas_quads_cover_the_canvas_in_target_ndc() {
    // A 3000x1000 canvas (two atlases wide) drawn 1:1 filling the target.
    let target = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(3000.0, 1000.0));
    let placement = Placement {
        canvas_size: egui::vec2(3000.0, 1000.0),
        zoom: 1.0,
        origin: target.min,
        center: target.center(),
        rotation: 0.0,
        flip: false,
        target,
        wrap: false,
    };
    let quads = atlas_quads(&placement, 2, 1);
    assert_eq!(quads.len(), 2);
    // Wrap-around: the canvas repeated round itself, the first repeat
    // one canvas to the right of it.
    let wrapped = atlas_quads(
        &Placement {
            wrap: true,
            ..placement
        },
        2,
        1,
    );
    assert_eq!(wrapped.len(), 18);
    let right_of = &wrapped[2 * 5];
    assert!((right_of.corners[0][0] - quads[0].corners[0][0] - 2.0).abs() < 1e-4);

    let split = 2.0 * ATLAS_SIZE as f32 / 3000.0 - 1.0;
    let close = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5;
    let left = quads[0];
    assert_eq!(left.atlas, 0);
    assert!(close(left.corners[0], [-1.0, 1.0]));
    assert!(close(left.corners[2], [split, -1.0]));
    let texture = ATLAS_TEXTURE_SIZE as f32;
    let border = ATLAS_BORDER as f32 / texture;
    assert_eq!(left.uvs[0], [border, border]);
    assert_eq!(
        left.uvs[2],
        [
            border + ATLAS_SIZE as f32 / texture,
            border + 1000.0 / texture
        ]
    );

    let right = quads[1];
    assert_eq!(right.atlas, 1);
    assert!(close(right.corners[0], [split, 1.0]));
    assert!(close(right.corners[2], [1.0, -1.0]));
    assert_eq!(
        right.uvs[2],
        [
            border + (3000.0 - ATLAS_SIZE as f32) / texture,
            border + 1000.0 / texture
        ]
    );
}

#[test]
fn tile_destinations_mirror_edges_into_neighbouring_atlas_borders() {
    let cache = RenderCache::new(4096, 4096); // 2x2 atlases
    let b = ATLAS_BORDER;
    let edge = ATLAS_SIZE - TILE_SIZE;

    // Bottom-right tile of atlas 0: its slot, plus the right, bottom and
    // diagonal neighbours' borders.
    let corner = tile_destinations(&cache, 31, 31, [TILE_SIZE, TILE_SIZE]);
    let strip = TILE_SIZE - b;
    assert_eq!(
        corner,
        vec![
            Destination {
                atlas: 0,
                dst: [b + edge, b + edge],
                src: [0, 0],
                size: [TILE_SIZE, TILE_SIZE]
            },
            Destination {
                atlas: 1,
                dst: [0, b + edge],
                src: [strip, 0],
                size: [b, TILE_SIZE]
            },
            Destination {
                atlas: 2,
                dst: [b + edge, 0],
                src: [0, strip],
                size: [TILE_SIZE, b]
            },
            Destination {
                atlas: 3,
                dst: [0, 0],
                src: [strip, strip],
                size: [b, b]
            },
        ]
    );

    // Left-edge tile of atlas 1 mirrors into atlas 0's right border only.
    let left = tile_destinations(&cache, 32, 0, [TILE_SIZE, TILE_SIZE]);
    assert_eq!(
        left,
        vec![
            Destination {
                atlas: 0,
                dst: [b + ATLAS_SIZE, b],
                src: [0, 0],
                size: [b, TILE_SIZE]
            },
            Destination {
                atlas: 1,
                dst: [b, b],
                src: [0, 0],
                size: [TILE_SIZE, TILE_SIZE]
            },
        ]
    );
}

/// Every atlas texel an upload set writes, keyed by (atlas, level, x, y).
fn texels(uploads: &[TileUpload]) -> std::collections::HashMap<(usize, u32, u32, u32), [u8; 4]> {
    let mut out = std::collections::HashMap::new();
    for u in uploads {
        for y in 0..u.height {
            for x in 0..u.width {
                let i = ((y * u.width + x) * 4) as usize;
                let px = [
                    u.pixels[i],
                    u.pixels[i + 1],
                    u.pixels[i + 2],
                    u.pixels[i + 3],
                ];
                out.insert((u.atlas, u.level, u.x + x, u.y + y), px);
            }
        }
    }
    out
}

#[test]
fn damaged_rect_uploads_match_the_full_tile_upload() {
    // A corner tile mirrored into three neighbouring atlases' borders.
    let cache = RenderCache::new(4096, 4096);
    let full = [TILE_SIZE, TILE_SIZE];
    let mut tile = egui::ColorImage::new(full, Color32::TRANSPARENT);
    for (i, px) in tile.pixels.iter_mut().enumerate() {
        *px = Color32::from_rgba_premultiplied((i % 251) as u8, (i / 251) as u8, 7, 255);
    }
    for level in [0u32, 1] {
        let full_img = downsample(&tile, level);
        let whole = texels(&tile_uploads(
            &cache,
            31,
            31,
            full,
            [0, 0, TILE_SIZE, TILE_SIZE],
            &full_img,
            level,
        ));
        for rect in [
            [8, 16, 24, 40],
            [TILE_SIZE - 6, 0, TILE_SIZE, 10],
            [0, TILE_SIZE - 4, TILE_SIZE, TILE_SIZE],
        ] {
            // Crop the damaged part, as the redraw does.
            let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
            let mut part = egui::ColorImage::new([w, h], Color32::TRANSPARENT);
            for y in 0..h {
                for x in 0..w {
                    part.pixels[y * w + x] = tile.pixels[(rect[1] + y) * TILE_SIZE + rect[0] + x];
                }
            }
            let part_img = downsample(&part, level);
            let partial = texels(&tile_uploads(&cache, 31, 31, full, rect, &part_img, level));
            assert!(!partial.is_empty(), "rect {rect:?} level {level}");
            for (key, px) in &partial {
                assert_eq!(
                    whole.get(key),
                    Some(px),
                    "rect {rect:?} level {level} texel {key:?}"
                );
            }
        }
    }
}

#[test]
fn preview_uploads_are_scaled_to_their_level() {
    let cache = RenderCache::new(4096, 4096);
    let full = [TILE_SIZE, TILE_SIZE];
    let img = egui::ColorImage::new([TILE_SIZE / 4, TILE_SIZE / 4], Color32::RED);
    let uploads = tile_uploads(&cache, 31, 31, full, [0, 0, TILE_SIZE, TILE_SIZE], &img, 2);
    let summary: Vec<_> = uploads
        .iter()
        .map(|u| (u.atlas, u.level, u.x, u.y, u.width, u.height))
        .collect();
    let (slot, border) = (
        (ATLAS_BORDER + ATLAS_SIZE - TILE_SIZE) as u32 / 4,
        ATLAS_BORDER as u32 / 4,
    );
    assert_eq!(
        summary,
        vec![
            (0, 2, slot, slot, 16, 16),
            (1, 2, 0, slot, border, 16),
            (2, 2, slot, 0, 16, border),
            (3, 2, 0, 0, border, border)
        ]
    );
    assert!(
        uploads
            .iter()
            .all(|u| u.pixels.len() == (u.width * u.height * 4) as usize)
    );
}

#[test]
fn downsample_averages_in_linear_light() {
    let mut img = egui::ColorImage::new([2, 2], Color32::BLACK);
    img.pixels[0] = Color32::WHITE;
    img.pixels[3] = Color32::WHITE;
    let half = downsample(&img, 1);
    assert_eq!(half.size, [1, 1]);
    // Half white in linear light is sRGB ~188, not the naive 128.
    let [r, g, b, a] = half.pixels[0].to_array();
    assert!((186..=189).contains(&r) && r == g && g == b, "{r}");
    assert_eq!(a, 255);
}

#[test]
fn visible_tile_range_covers_only_tiles_under_clip() {
    let identity = |p: egui::Pos2| p;
    let clip = egui::Rect::from_min_max(egui::pos2(70.0, 10.0), egui::pos2(130.0, 20.0));
    assert_eq!(visible_tile_range(clip, identity, 4, 3), (1..3, 0..1));

    let offscreen = egui::Rect::from_min_max(egui::pos2(-50.0, -50.0), egui::pos2(-1.0, -1.0));
    let (xs, ys) = visible_tile_range(offscreen, identity, 4, 3);
    assert!(xs.is_empty() && ys.is_empty());

    let whole = egui::Rect::from_min_max(egui::pos2(-1e4, -1e4), egui::pos2(1e4, 1e4));
    assert_eq!(visible_tile_range(whole, identity, 4, 3), (0..4, 0..3));
}

#[test]
fn preview_level_never_exceeds_the_level_the_gpu_samples() {
    assert_eq!(
        preview_level(false, 0.1, 1.0),
        0,
        "full resolution when not drawing"
    );
    assert_eq!(preview_level(true, 1.0, 1.0), 0);
    assert_eq!(
        preview_level(true, 0.5, 1.0),
        0,
        "exactly 2 texels/pixel keeps a margin"
    );
    assert_eq!(preview_level(true, 0.3, 1.0), 1);
    // The user's case: a 1.375x display at 0.1 zoom samples ~log2(7.3) = 2.9,
    // so level 3 would leave level 2 stale.
    assert_eq!(preview_level(true, 0.1, 1.375), 2);
    assert_eq!(preview_level(true, 0.02, 1.0), MIP_LEVELS - 1);
    for zoom in [0.05f32, 0.1, 0.2, 0.33, 0.5, 0.7] {
        for ppp in [1.0f32, 1.25, 1.5, 2.0] {
            let sampled = (1.0 / (zoom * ppp)).log2().max(0.0);
            assert!(
                preview_level(true, zoom, ppp) as f32 <= sampled.floor(),
                "zoom {zoom} ppp {ppp}"
            );
        }
    }
}

#[test]
fn flipped_view_maps_to_screen_and_back() {
    use super::ScreenMap;
    use crate::canvas::Canvas;
    use eframe::egui::{Color32, Vec2, pos2, vec2};
    let mut app = crate::project::tests::test_app_pub(Canvas::new(300, 200, Color32::WHITE, 64));
    app.viewport.zoom = 1.7;
    app.viewport.rotation = 0.6;
    app.viewport.flip_x = true;
    let origin = pos2(40.0, 25.0);
    let center = origin + vec2(300.0, 200.0) * 1.7 * 0.5;
    let (sin, cos) = 0.6f32.sin_cos();
    let map = ScreenMap {
        origin,
        center,
        zoom: 1.7,
        cos,
        sin,
        flip: Some(300.0),
    };
    for p in [
        Vec2::new(0.0, 0.0),
        Vec2::new(250.0, 30.0),
        Vec2::new(12.5, 199.0),
    ] {
        let back = app.screen_to_canvas_raw(map.to_screen(p), origin, center);
        assert!((back - p).length() < 1e-3, "{p:?} came back as {back:?}");
        let back = map.to_canvas(map.to_screen(p));
        assert!((back - p).length() < 1e-3, "{p:?} came back as {back:?}");
    }
    // Flipped: the canvas's left edge shows on the right.
    let left = map.to_screen(Vec2::new(0.0, 100.0));
    let right = map.to_screen(Vec2::new(300.0, 100.0));
    let unrotated = |q: eframe::egui::Pos2| (q - center).x * cos + (q - center).y * sin;
    assert!(unrotated(left) > unrotated(right));
}

/// A `size`² app whose layer 1 is painted all over (a colour field with
/// black lines, as `bench_api`'s `painted_app`), seen whole in a
/// 1200×900 window.
fn painted(size: usize) -> PainterApp {
    let canvas = crate::canvas::Canvas::new(size, size, Color32::WHITE, TILE_SIZE);
    let tiles = size.div_ceil(TILE_SIZE);
    for ty in 0..tiles {
        for tx in 0..tiles {
            let tile: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
                .map(|i| {
                    let (x, y) = (
                        tx * TILE_SIZE + i % TILE_SIZE,
                        ty * TILE_SIZE + i / TILE_SIZE,
                    );
                    if x % 50 < 3 || y % 50 < 3 {
                        Color32::BLACK
                    } else {
                        Color32::from_rgb((x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8)
                    }
                })
                .collect();
            canvas.set_layer_tile_data(1, tx as i32, ty as i32, tile);
        }
    }
    let mut app = crate::project::tests::test_app_pub(canvas);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    app.workspace.pool = std::sync::Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap(),
    );
    app.recreate_render_cache(size, size);
    app.canvas_mut().active_layer_idx = 1;
    app.viewport.zoom = 880.0 / size as f32;
    app.viewport.offset = eframe::egui::Vec2::new(150.0, 10.0);
    app.workspace.auto_fit = false;
    app
}

#[test]
#[ignore = "timing: cargo test --release -- --ignored --nocapture"]
fn adjustment_preview_timing_4k() {
    use crate::canvas::filters::Filter;
    let mut app = painted(4000);
    let mut screen = ScreenSim::new();
    screen.settle(&mut app);
    let hue = |h: f32| Filter::HueSaturation {
        hue: h,
        saturation: 0.1,
        lightness: 0.0,
    };
    let report = |what: &str, ticks: &[(usize, std::time::Duration)]| {
        let frames: usize = ticks.iter().map(|t| t.0).sum();
        let worst = ticks.iter().map(|t| t.1).max().unwrap_or_default();
        let mean = ticks.iter().map(|t| t.1).sum::<std::time::Duration>() / ticks.len() as u32;
        eprintln!(
            "{what}: {:.1} frames a step, {mean:?} a step (worst {worst:?})",
            frames as f32 / ticks.len() as f32
        );
    };
    // A filter's slider dragged: each step, the frames until the
    // screen shows it.
    app.filter_open(hue(10.0));
    screen.settle(&mut app);
    screen.dragging = true;
    let ticks: Vec<_> = (0..10)
        .map(|step| {
            let session = app.workspace.filter.session.as_mut().unwrap();
            session.filter = hue(20.0 + step as f32);
            session.dirty = true;
            screen.settle_counting(&mut app)
        })
        .collect();
    report("filter slider step", &ticks);
    screen.dragging = false;
    let (frames, spent) = screen.settle_counting(&mut app);
    eprintln!("filter slider let go: exact in {frames} frames, {spent:?}");
    app.filter_cancel();
    screen.settle(&mut app);

    // An adjustment layer's slider dragged.
    app.add_adjustment_layer(hue(10.0));
    screen.settle(&mut app);
    let idx = app.canvas.active_layer_idx;
    screen.dragging = true;
    let ticks: Vec<_> = (0..10)
        .map(|step| {
            app.workspace.filter.adjusting = true;
            app.canvas_mut().layers[idx].adjustment = Some(hue(20.0 + step as f32));
            app.mark_all_tiles_dirty();
            screen.settle_counting(&mut app)
        })
        .collect();
    report("adjustment slider step", &ticks);
    screen.dragging = false;
    let (frames, spent) = screen.settle_counting(&mut app);
    eprintln!("adjustment slider let go: exact in {frames} frames, {spent:?}");
}

/// Liquify as a pen drives it, frame by frame: what the UI thread
/// spends on the field, drawing the layer and the screen update.
#[test]
#[ignore = "timing: cargo test --release -- --ignored --nocapture"]
fn liquify_frame_timing_4k() {
    use crate::app::tools::Tool;
    use crate::canvas::liquify::LiquifyMode;
    use std::time::{Duration, Instant};
    for (view, zoom) in [("fit", None), ("100%", Some(1.0))] {
        for (mode, radius, speed) in [
            (LiquifyMode::Push, 60.0, 8.0),
            (LiquifyMode::Push, 150.0, 8.0),
            (LiquifyMode::Push, 300.0, 8.0),
            (LiquifyMode::Push, 150.0, 40.0),
            (LiquifyMode::TwirlCw, 150.0, 8.0),
            (LiquifyMode::TwirlCw, 300.0, 0.0),
        ] {
            let mut app = painted(4000);
            if let Some(z) = zoom {
                app.viewport.zoom = z;
                app.viewport.offset = eframe::egui::Vec2::new(-1400.0, -1550.0);
            }
            let mut screen = ScreenSim::new();
            if zoom.is_some() {
                // What's on screen uploaded, then one frame a call.
                screen.unsettled_ok = true;
                screen.max_frames = 300;
                screen.settle(&mut app);
                screen.max_frames = 1;
            } else {
                screen.settle(&mut app);
            }
            app.active_tool = Tool::Liquify;
            app.workspace.liquify.mode = mode;
            app.workspace.liquify.radius = radius;
            let mut pos = eframe::egui::Vec2::new(1800.0, 2000.0);
            app.liquify_press(pos);
            let frames = 60;
            let (mut field, mut layer, mut shown) =
                (Duration::ZERO, Duration::ZERO, Duration::ZERO);
            let mut worst = Duration::ZERO;
            for _ in 0..frames {
                // A 240 Hz pen at 60 fps: four samples a frame.
                let t = Instant::now();
                for _ in 0..4 {
                    pos.x += speed / 4.0;
                    app.liquify_drag(pos);
                }
                if mode.is_continuous() {
                    app.liquify_hold(1.0 / 60.0);
                }
                let f = t.elapsed();
                let t = Instant::now();
                // The screen's frame draws it (zoomed out, a preview).
                let l = t.elapsed();
                let (_, s) = screen.settle_counting(&mut app);
                (field, layer, shown) = (field + f, layer + l, shown + s);
                worst = worst.max(f + l + s);
            }
            app.liquify_release();
            app.liquify_commit();
            let n = frames as u32;
            eprintln!(
                "liquify {view:>4} {mode:?} r{radius} {speed}px/frame: field {:>7.2?}  layer {:>7.2?}  screen {:>7.2?}  = {:>7.2?}/frame (worst {:.2?})",
                field / n,
                layer / n,
                shown / n,
                (field + layer + shown) / n,
                worst
            );
        }
    }
}
