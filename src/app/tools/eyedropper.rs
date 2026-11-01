//! The Eyedropper: what it samples (one pixel or an average around it, of
//! everything visible or the active layer only), and the ring shown while
//! picking: the new colour above, the colour before below.

use crate::PainterApp;
use crate::canvas::storage::SampleLayers;
use eframe::egui::{self, Color32, Vec2};

/// What the eyedropper looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PickFrom {
    /// The picture as it shows.
    Visible,
    /// The active layer alone (its transparent pixels pick nothing).
    Layer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EyedropperSettings {
    /// Side of the square averaged, in pixels (1, 3, 5 or 11).
    pub size: u8,
    pub from: PickFrom,
}

impl Default for EyedropperSettings {
    fn default() -> Self {
        Self {
            size: 1,
            from: PickFrom::Visible,
        }
    }
}

/// The sample sizes offered.
pub const SIZES: [(u8, &str); 4] = [(1, "1 px"), (3, "3×3"), (5, "5×5"), (11, "11×11")];

#[derive(Default)]
pub struct EyedropperState {
    pub settings: EyedropperSettings,
    /// While picking: the colour before it began (the ring's lower half).
    pub before: Option<Color32>,
}

/// The average colour of premultiplied `pixels`, unpremultiplied and
/// opaque; `None` if they're all transparent.
fn average(pixels: &[Color32]) -> Option<Color32> {
    let mut sum = [0u64; 4];
    for p in pixels {
        for (s, c) in sum.iter_mut().zip(p.to_array()) {
            *s += c as u64;
        }
    }
    let a = sum[3];
    if a == 0 {
        return None;
    }
    // Premultiplied sums over the alpha sum: transparent pixels count for
    // nothing, half-covered ones for half.
    let channel = |s: u64| ((s * 255 + a / 2) / a).min(255) as u8;
    Some(Color32::from_rgb(
        channel(sum[0]),
        channel(sum[1]),
        channel(sum[2]),
    ))
}

impl PainterApp {
    /// Set the brush color to the color at `pos` (as the Eyedropper's
    /// settings say), keeping the brush's own alpha.
    pub(crate) fn pick_color(&mut self, pos: Vec2) {
        // What vector layers will show, not their old pixels.
        if self.vector_busy() {
            self.settle_vectors();
        }
        let settings = self.workspace.eyedropper.settings;
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let r = (settings.size.max(1) as i32 - 1) / 2;
        let (x, y) = (
            (pos.x.floor() as i32).clamp(0, w - 1),
            (pos.y.floor() as i32).clamp(0, h - 1),
        );
        let (x0, y0) = ((x - r).max(0), (y - r).max(0));
        let (x1, y1) = ((x + r + 1).min(w), (y + r + 1).min(h));
        let (sw, sh) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let pixels = match settings.from {
            PickFrom::Visible => {
                let mut image = egui::ColorImage::new([sw, sh], Color32::TRANSPARENT);
                // Draft layers aren't sampled.
                let view = self.canvas.without_drafts();
                view.as_ref()
                    .unwrap_or(&self.canvas)
                    .write_region_to_color_image(x0 as usize, y0 as usize, sw, sh, &mut image, 1);
                image.pixels
            }
            PickFrom::Layer => self.canvas.render_sample(
                SampleLayers::Layer(self.canvas.active_layer_idx),
                x0,
                y0,
                sw,
                sh,
            ),
        };
        let Some(sample) = average(&pixels) else {
            return;
        };
        let color = &mut self.brush_state.brush.brush_options.color;
        self.workspace.eyedropper.before.get_or_insert(*color);
        *color = Color32::from_rgba_unmultiplied(sample.r(), sample.g(), sample.b(), color.a());
        self.brush_state.brush_preview.dirty = true;
    }

    /// The ring around the pointer while picking: the picked colour on top,
    /// the one before underneath.
    pub(crate) fn draw_pick_ring(&self, ctx: &egui::Context, painter: &egui::Painter) {
        let Some(before) = self.workspace.eyedropper.before else {
            return;
        };
        if !ctx.input(|i| i.pointer.primary_down()) {
            return;
        }
        let Some(center) = ctx.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        let opaque = |c: Color32| Color32::from_rgb(c.r(), c.g(), c.b());
        let now = opaque(self.brush_state.brush.brush_options.color);
        let (radius, width) = (54.0, 20.0);
        // Two half rings as thick arcs, outlined in dark and light so they
        // show on any picture.
        let arc = |from: f32, to: f32| -> Vec<egui::Pos2> {
            (0..=32)
                .map(|i| {
                    let t = from + (to - from) * i as f32 / 32.0;
                    center + radius * egui::vec2(t.cos(), t.sin())
                })
                .collect()
        };
        use std::f32::consts::PI;
        let outline = egui::Stroke::new(width + 4.0, Color32::from_black_alpha(140));
        painter.add(egui::Shape::closed_line(arc(0.0, 2.0 * PI), outline));
        painter.add(egui::Shape::line(
            arc(PI, 2.0 * PI),
            egui::Stroke::new(width, now),
        ));
        painter.add(egui::Shape::line(
            arc(0.0, PI),
            egui::Stroke::new(width, opaque(before)),
        ));
        let edge = egui::Stroke::new(1.0_f32, Color32::from_white_alpha(160));
        for r in [radius - width / 2.0 - 1.0, radius + width / 2.0 + 1.0] {
            painter.circle_stroke(center, r, edge);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_average_weighs_pixels_by_their_alpha() {
        let red = Color32::from_rgb(255, 0, 0);
        let half_blue = Color32::from_rgba_premultiplied(0, 0, 128, 128);
        assert_eq!(average(&[Color32::TRANSPARENT; 4]), None);
        assert_eq!(average(&[red, Color32::TRANSPARENT]), Some(red));
        // Two parts red to one of blue.
        let mixed = average(&[red, half_blue]).unwrap();
        assert_eq!((mixed.r(), mixed.b()), (170, 85));
    }

    #[test]
    fn a_bigger_sample_averages_around_the_point() {
        let mut app = crate::project::tests::test_app_pub(crate::canvas::Canvas::new(
            64,
            64,
            Color32::WHITE,
            64,
        ));
        app.canvas_mut().active_layer_idx = 1;
        // Black from x = 10 on; the layer is see-through left of it.
        let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
        for y in 0..64 {
            for x in 10..64 {
                tile[y * 64 + x] = Color32::BLACK;
            }
        }
        app.canvas_mut().set_layer_tile_data(1, 0, 0, tile);
        let pick = |app: &mut PainterApp, size, from, x| {
            app.workspace.eyedropper.settings = EyedropperSettings { size, from };
            app.brush_state.brush.brush_options.color = Color32::RED;
            app.pick_color(Vec2::new(x, 20.5));
            app.brush_state.brush.brush_options.color
        };
        assert_eq!(pick(&mut app, 1, PickFrom::Visible, 9.5), Color32::WHITE);
        // 3×3 over x = 9..=11: one column white, two black.
        let c = pick(&mut app, 3, PickFrom::Visible, 10.5);
        assert!((80..=90).contains(&c.r()), "a third white: {c:?}");
        // The layer alone: its transparent column counts for nothing.
        assert_eq!(pick(&mut app, 3, PickFrom::Layer, 10.5), Color32::BLACK);
        assert_eq!(
            pick(&mut app, 1, PickFrom::Layer, 5.5),
            Color32::RED,
            "nothing there: the colour stays"
        );
        assert_eq!(app.workspace.eyedropper.before, Some(Color32::RED));
    }
}
