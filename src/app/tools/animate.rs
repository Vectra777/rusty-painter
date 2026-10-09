//! The Animate tool: move, scale and turn the selected layer on the canvas,
//! or (Alt+drag) move the point it turns about, keying it at the frame showing (an
//! animated layer moves with all its drawings). Its path across the
//! timeline shows, a dot a frame and a diamond at each key.

use crate::PainterApp;
use crate::canvas::motion::{Motion, Prop};
use crate::canvas::rig::Affine;
use crate::ui::style::{TEXT_STRONG, accent};
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};

/// What a press on the canvas grabbed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grab {
    Move,
    /// A corner: scale about the anchor.
    Scale,
    Rotate,
    Anchor,
}

/// A drag under way.
#[derive(Clone, Copy, Debug)]
pub struct AnimateDrag {
    grab: Grab,
    target: usize,
    from: Vec2,
    position: [f32; 2],
    scale: [f32; 2],
    rotation: f32,
    anchor: [f32; 2],
    /// Where the anchor showed when the drag began (canvas).
    anchor_shown: Vec2,
    /// The transform it's moved in (its folder's or the layer it follows).
    base: Affine,
}

/// The selected layer's box: its corners (canvas, clockwise from top
/// left), the rotate handle and the anchor, where they show.
pub struct Handles {
    pub corners: [Vec2; 4],
    pub rotate: Vec2,
    pub anchor: Vec2,
}

fn v([x, y]: [f32; 2]) -> Vec2 {
    Vec2::new(x, y)
}

impl PainterApp {
    /// The layer the Animate tool works on, and its handles (sized for
    /// the view's zoom).
    pub(crate) fn animate_handles(&self) -> Option<(usize, Handles)> {
        let target = self.motion_target(self.canvas.active_layer_idx)?;
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        let [x0, y0, x1, y1] = self.content_rect_cached(target).unwrap_or([0.0, 0.0, w, h]);
        let (world, _) = self.canvas.world_motion(target);
        let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].map(|p| v(world.apply(p)));
        let anchor_local = (self.canvas.layers[target].motion.as_ref())
            .map_or([(x0 + x1) / 2.0, (y0 + y1) / 2.0], |m| {
                m.value(Prop::Anchor, self.canvas.time as f32)
            });
        let top = (corners[0] + corners[1]) / 2.0;
        let up = (corners[0] - corners[3]).normalized();
        let up = if up.is_finite() {
            up
        } else {
            Vec2::new(0.0, -1.0)
        };
        let rotate = top + up * (28.0 / self.viewport.zoom.max(0.01));
        Some((
            target,
            Handles {
                corners,
                rotate,
                anchor: v(world.apply(anchor_local)),
            },
        ))
    }

    /// `alt`: move the point it turns about instead.
    pub(crate) fn animate_press(&mut self, pos: Vec2, alt: bool) {
        let Some((target, handles)) = self.animate_handles() else {
            self.report("Pick a layer to animate (not the background)".to_string());
            return;
        };
        let reach = 10.0 / self.viewport.zoom.max(0.01);
        let near = |p: Vec2| (p - pos).length() <= reach;
        let grab = if alt {
            Grab::Anchor
        } else if near(handles.rotate) {
            Grab::Rotate
        } else if handles.corners.iter().any(|&c| near(c)) {
            Grab::Scale
        } else {
            Grab::Move
        };
        let (world, _) = self.canvas.world_motion(target);
        let t = self.canvas.time as f32;
        let motion = (self.canvas.layers[target].motion.as_deref().cloned())
            .unwrap_or_else(|| Motion::new(self.layer_anchor(target)));
        let (local, _) = motion.local(t);
        let base = local
            .inverse()
            .map_or(Affine::IDENTITY, |inv| world.then(&inv));
        self.workspace.motion.drag = Some(AnimateDrag {
            grab,
            target,
            from: pos,
            position: motion.value(Prop::Position, t),
            scale: motion.value(Prop::Scale, t),
            rotation: motion.value(Prop::Rotation, t)[0],
            anchor: motion.value(Prop::Anchor, t),
            anchor_shown: handles.anchor,
            base,
        });
    }

    /// Where layer `i` turns about (its anchor, or the middle of it).
    fn layer_anchor(&self, i: usize) -> [f32; 2] {
        match self.canvas.layers[i].motion.as_ref() {
            Some(m) => m.value(Prop::Anchor, self.canvas.time as f32),
            None => match self.content_rect_cached(i) {
                Some([x0, y0, x1, y1]) => [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
                None => [
                    self.canvas.width() as f32 / 2.0,
                    self.canvas.height() as f32 / 2.0,
                ],
            },
        }
    }

    /// `snap`: Shift, turns by 15° steps and scales both ways alike.
    pub(crate) fn animate_drag(&mut self, pos: Vec2, snap: bool) {
        let Some(d) = self.workspace.motion.drag else {
            return;
        };
        let t = self.canvas.time;
        // A move on the canvas as a move where the layer's keys are.
        let [a, b, c, dd] = d.base.m;
        let det = a * dd - b * c;
        let to_base = |delta: Vec2| {
            if det.abs() < 1e-9 {
                delta
            } else {
                Vec2::new(
                    (dd * delta.x - b * delta.y) / det,
                    (-c * delta.x + a * delta.y) / det,
                )
            }
        };
        match d.grab {
            Grab::Move => {
                let moved = to_base(pos - d.from);
                let value = [d.position[0] + moved.x, d.position[1] + moved.y];
                self.motion_edit(d.target, |m| m.set(Prop::Position, t, value));
            }
            Grab::Rotate => {
                let (r0, r1) = (d.from - d.anchor_shown, pos - d.anchor_shown);
                let mut turn = d.rotation + (r1.y.atan2(r1.x) - r0.y.atan2(r0.x)).to_degrees();
                if snap {
                    turn = (turn / 15.0).round() * 15.0;
                }
                self.motion_edit(d.target, |m| m.set(Prop::Rotation, t, [turn, 0.0]));
            }
            Grab::Scale => {
                let (r0, r1) = (
                    (d.from - d.anchor_shown).length(),
                    (pos - d.anchor_shown).length(),
                );
                let k = if r0 > 1e-3 { r1 / r0 } else { 1.0 };
                let value = if snap {
                    let s = (d.scale[0] * k + d.scale[1] * k) / 2.0;
                    [s, s]
                } else {
                    [d.scale[0] * k, d.scale[1] * k]
                };
                self.motion_edit(d.target, |m| m.set(Prop::Scale, t, value));
            }
            Grab::Anchor => {
                // The anchor moves over the picture, which stays put: the
                // position keys make up the difference.
                let tf = t as f32;
                let current = self.canvas.layers[d.target].motion.as_deref().cloned();
                let world = self.canvas.world_motion(d.target).0;
                let Some(inv) = world.inverse() else {
                    return;
                };
                let new_anchor = inv.apply([pos.x, pos.y]);
                let old = current.map_or(d.anchor, |m| m.value(Prop::Anchor, tf));
                self.motion_edit(d.target, |m| {
                    let (local, _) = m.local(tf);
                    let lm = local.m;
                    let delta = [new_anchor[0] - old[0], new_anchor[1] - old[1]];
                    // anchor + d - M·anchor stays the same.
                    let shift = [
                        lm[0] * delta[0] + lm[1] * delta[1] - delta[0],
                        lm[2] * delta[0] + lm[3] * delta[1] - delta[1],
                    ];
                    // A keyed pivot: keyed here, and so is where it is.
                    if !m.anchor_keys.is_empty() {
                        let [dx, dy] = m.value(Prop::Position, tf);
                        m.set(Prop::Anchor, t, new_anchor);
                        m.set(Prop::Position, t, [dx + shift[0], dy + shift[1]]);
                        return;
                    }
                    m.anchor = new_anchor;
                    for k in &mut m.position {
                        k.value = [k.value[0] + shift[0], k.value[1] + shift[1]];
                    }
                    if m.position.is_empty() && (shift[0].abs() > 1e-4 || shift[1].abs() > 1e-4) {
                        m.set(Prop::Position, t, shift);
                    }
                });
            }
        }
    }

    pub(crate) fn animate_release(&mut self) {
        self.workspace.motion.drag = None;
        self.motion_edit_done();
        self.layer_state.thumbnails_dirty = true;
    }

    /// The cursor over the canvas with the Animate tool.
    pub(crate) fn animate_cursor(&self, pos: Vec2, alt: bool) -> egui::CursorIcon {
        let grab = match self.workspace.motion.drag {
            Some(d) => Some(d.grab),
            None => self.animate_handles().map(|(_, h)| {
                let reach = 10.0 / self.viewport.zoom.max(0.01);
                let near = |p: Vec2| (p - pos).length() <= reach;
                if alt {
                    Grab::Anchor
                } else if near(h.rotate) {
                    Grab::Rotate
                } else if h.corners.iter().any(|&c| near(c)) {
                    Grab::Scale
                } else {
                    Grab::Move
                }
            }),
        };
        match grab {
            Some(Grab::Move) => egui::CursorIcon::Move,
            Some(Grab::Scale) => egui::CursorIcon::ResizeNwSe,
            Some(Grab::Rotate) => egui::CursorIcon::Alias,
            Some(Grab::Anchor) => egui::CursorIcon::Crosshair,
            None => egui::CursorIcon::NotAllowed,
        }
    }
}

/// The box, handles and path of the layer the Animate tool works on.
pub(crate) fn draw_animate(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> Pos2,
) {
    if !matches!(app.active_tool, crate::app::tools::Tool::Animate) {
        return;
    }
    let Some((target, h)) = app.animate_handles() else {
        return;
    };
    let shadow = Stroke::new(3.0_f32, Color32::from_black_alpha(120));
    let line = Stroke::new(1.25_f32, accent());
    // The path: where the anchor is each frame, a diamond at each key.
    if let Some(motion) = app.canvas.layers[target].motion.as_deref()
        && motion.keys(Prop::Position).len() > 1
    {
        let frames = motion.key_frames(Prop::Position);
        let (first, last) = (frames[0], frames[frames.len() - 1]);
        let at = |t: u32| {
            let (world, _) = app.canvas.world_motion_at(target, t as f32, 0);
            to_screen(v(world.apply(motion.value(Prop::Anchor, t as f32))))
        };
        let points: Vec<Pos2> = (first..=last.min(first + 2000)).map(at).collect();
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(1.0_f32, accent().gamma_multiply(0.6)),
        ));
        for (k, p) in points.iter().enumerate() {
            let t = first + k as u32;
            if frames.contains(&t) {
                let r = 5.0;
                let diamond = vec![
                    *p + Vec2::new(0.0, -r),
                    *p + Vec2::new(r, 0.0),
                    *p + Vec2::new(0.0, r),
                    *p + Vec2::new(-r, 0.0),
                ];
                painter.add(egui::Shape::convex_polygon(
                    diamond,
                    accent(),
                    Stroke::new(1.0_f32, Color32::BLACK),
                ));
            } else {
                painter.circle_filled(*p, 1.8, accent().gamma_multiply(0.8));
            }
        }
    }
    let corners: Vec<Pos2> = h.corners.iter().map(|&c| to_screen(c)).collect();
    let mut closed = corners.clone();
    closed.push(corners[0]);
    painter.add(egui::Shape::line(closed.clone(), shadow));
    painter.add(egui::Shape::line(closed, line));
    let top = Pos2::new(
        (corners[0].x + corners[1].x) / 2.0,
        (corners[0].y + corners[1].y) / 2.0,
    );
    let rotate = to_screen(h.rotate);
    painter.line_segment([top, rotate], line);
    painter.circle_filled(rotate, 5.0, Color32::WHITE);
    painter.circle_stroke(rotate, 5.0, Stroke::new(1.5_f32, accent()));
    for c in &corners {
        let r = egui::Rect::from_center_size(*c, egui::vec2(8.0, 8.0));
        painter.rect_filled(r, 1.0, Color32::WHITE);
        painter.rect_stroke(r, 1.0, Stroke::new(1.5_f32, accent()));
    }
    let a = to_screen(h.anchor);
    painter.circle_stroke(a, 6.0, shadow);
    painter.circle_stroke(a, 6.0, Stroke::new(1.5_f32, TEXT_STRONG));
    for (d0, d1) in [
        (Vec2::new(-10.0, 0.0), Vec2::new(10.0, 0.0)),
        (Vec2::new(0.0, -10.0), Vec2::new(0.0, 10.0)),
    ] {
        painter.line_segment([a + d0, a + d1], Stroke::new(1.0_f32, TEXT_STRONG));
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::motion::Prop;
    use eframe::egui::{Color32, Vec2};

    #[test]
    fn dragging_the_layer_keys_where_it_goes() {
        let canvas = Canvas::new(128, 128, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app.active_tool = crate::app::tools::Tool::Animate;
        app.go_to_frame(4);
        app.animate_press(Vec2::new(30.0, 30.0), false);
        app.animate_drag(Vec2::new(50.0, 40.0), false);
        app.animate_release();
        let motion = app.canvas.layers[1].motion.as_deref().expect("keyed");
        assert_eq!(motion.key_frames(Prop::Position), [4]);
        assert_eq!(motion.value(Prop::Position, 4.0), [20.0, 10.0]);
        // Turned by the handle above the box: a quarter turn clockwise.
        let (_, h) = app.animate_handles().unwrap();
        app.animate_press(h.rotate, false);
        let r = (h.rotate - h.anchor).length();
        app.animate_drag(h.anchor + Vec2::new(r, 0.0), true);
        app.animate_release();
        let turn = app.canvas.layers[1]
            .motion
            .as_deref()
            .unwrap()
            .value(Prop::Rotation, 4.0)[0];
        assert!((turn - 90.0).abs() < 1e-3, "{turn}");
    }
}

/// Times an Animate drag on a 4K canvas (run with `--release --ignored`).
#[cfg(test)]
#[test]
#[ignore = "timing"]
fn animate_drag_timings() {
    use crate::canvas::Canvas;
    let ts = crate::app::document::TILE_SIZE;
    let canvas = Canvas::new(3840, 2160, Color32::WHITE, ts);
    for ty in 8..24 {
        for tx in 20..36 {
            canvas.set_layer_tile_data(1, tx, ty, vec![Color32::RED; ts * ts]);
        }
    }
    let mut app = crate::project::tests::test_app_pub(canvas);
    app.canvas_mut().active_layer_idx = 1;
    app.active_tool = crate::app::tools::Tool::Animate;
    app.animate_press(Vec2::new(1800.0, 1000.0), false);
    let mut events = std::time::Duration::ZERO;
    let mut frames = std::time::Duration::ZERO;
    for k in 0..30 {
        let start = std::time::Instant::now();
        for e in 0..4 {
            let x = 1800.0 + (k * 4 + e) as f32 * 3.0;
            app.animate_drag(Vec2::new(x, 1000.0), false);
        }
        events += start.elapsed();
        let start = std::time::Instant::now();
        app.show_motion_changes();
        frames += start.elapsed();
    }
    app.animate_release();
    eprintln!(
        "per event: {:?}, per frame: {:?}",
        events / 120,
        frames / 30
    );
}
