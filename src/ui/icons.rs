//! Small vector icons painted with egui shapes, so they stay crisp at any
//! scale and don't depend on which emoji glyphs the bundled font has.
//! Each icon is drawn on a 16×16 grid mapped into the target rect.

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    Brush,
    Eraser,
    SelectRect,
    SelectEllipse,
    Lasso,
    Transform,
    Eyedropper,
    Swap,
    Eye,
    EyeOff,
    Lock,
    Unlock,
    Trash,
    Plus,
    Undo,
    Redo,
    PanelLeft,
    PanelRight,
    Presets,
    Folder,
    Mask,
    SelectBrush,
    Bucket,
    Liquify,
    Palette,
    Smudge,
    Blur,
    Menu,
    Finger,
    Wand,
    ColorRange,
    Magnetic,
}

/// Maps 16×16 icon-grid coordinates into a square centered in `rect`.
struct Grid {
    origin: Pos2,
    unit: f32,
}

impl Grid {
    fn new(rect: Rect) -> Self {
        let side = rect.width().min(rect.height());
        let origin = rect.center() - egui::vec2(side, side) * 0.5;
        Self {
            origin,
            unit: side / 16.0,
        }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + egui::vec2(x, y) * self.unit
    }

    fn w(&self, width: f32) -> f32 {
        (width * self.unit).max(1.0)
    }
}

fn quad_bezier(a: Pos2, c: Pos2, b: Pos2, steps: usize) -> Vec<Pos2> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let u = 1.0 - t;
            Pos2::new(
                u * u * a.x + 2.0 * u * t * c.x + t * t * b.x,
                u * u * a.y + 2.0 * u * t * c.y + t * t * b.y,
            )
        })
        .collect()
}

fn ellipse_points(center: Pos2, rx: f32, ry: f32, steps: usize) -> Vec<Pos2> {
    (0..=steps)
        .map(|i| {
            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
            center + egui::vec2(a.cos() * rx, a.sin() * ry)
        })
        .collect()
}

fn arrow_head(painter: &egui::Painter, tip: Pos2, dir: egui::Vec2, size: f32, color: Color32) {
    let dir = dir.normalized();
    let normal = egui::vec2(-dir.y, dir.x);
    let base = tip - dir * size;
    painter.add(Shape::convex_polygon(
        vec![tip, base + normal * size * 0.7, base - normal * size * 0.7],
        color,
        Stroke::NONE,
    ));
}

/// Paints `icon` into `rect` using `color`.
pub(crate) fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let g = Grid::new(rect);
    let thin = Stroke::new(g.w(1.25), color);
    let line = |a: (f32, f32), b: (f32, f32), width: f32| {
        painter.line_segment(
            [g.p(a.0, a.1), g.p(b.0, b.1)],
            Stroke::new(g.w(width), color),
        );
    };

    match icon {
        Icon::Brush => {
            line((13.5, 2.5), (8.0, 8.0), 1.6);
            line((8.6, 6.4), (9.6, 7.4), 2.6);
            painter.circle_filled(g.p(6.0, 10.0), g.w(2.4), color);
            painter.add(Shape::convex_polygon(
                vec![g.p(4.0, 11.0), g.p(7.0, 12.0), g.p(2.0, 14.5)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Eraser => {
            let a = g.p(9.5, 2.5);
            let b = g.p(14.5, 7.5);
            let c = g.p(8.0, 14.0);
            let d = g.p(3.0, 9.0);
            let p1 = d.lerp(a, 0.5);
            let p2 = c.lerp(b, 0.5);
            painter.add(Shape::convex_polygon(
                vec![d, p1, p2, c],
                color,
                Stroke::NONE,
            ));
            painter.add(Shape::closed_line(vec![a, b, c, d], thin));
            line((9.0, 14.0), (14.5, 14.0), 1.25);
        }
        Icon::SelectRect => {
            let pts = vec![
                g.p(2.5, 2.5),
                g.p(13.5, 2.5),
                g.p(13.5, 13.5),
                g.p(2.5, 13.5),
                g.p(2.5, 2.5),
            ];
            painter.extend(Shape::dashed_line(&pts, thin, g.w(2.0), g.w(1.6)));
        }
        Icon::SelectEllipse => {
            let pts = ellipse_points(g.p(8.0, 8.0), g.w(6.0), g.w(6.0), 48);
            painter.extend(Shape::dashed_line(&pts, thin, g.w(2.0), g.w(1.6)));
        }
        Icon::Lasso => {
            let loop_pts = ellipse_points(g.p(8.5, 6.5), g.w(5.5), g.w(3.8), 40);
            painter.add(Shape::line(loop_pts, thin));
            let tail = quad_bezier(g.p(4.5, 9.0), g.p(3.0, 12.0), g.p(6.0, 14.0), 12);
            painter.add(Shape::line(tail, thin));
            painter.circle_filled(g.p(6.0, 14.0), g.w(1.2), color);
        }
        Icon::Wand => {
            // A wand with a sparkle at its tip.
            line((2.5, 13.5), (9.5, 6.5), 1.8);
            for (a, b) in [
                ((12.0, 1.5), (12.0, 4.5)),
                ((10.5, 3.0), (13.5, 3.0)),
                ((14.0, 6.5), (14.0, 8.5)),
                ((13.0, 7.5), (15.0, 7.5)),
                ((7.0, 1.5), (7.0, 3.5)),
                ((6.0, 2.5), (8.0, 2.5)),
            ] {
                line(a, b, 1.1);
            }
        }
        Icon::ColorRange => {
            // Three swatches, two picked out by a dashed outline.
            for (x, y) in [(2.0, 2.0), (9.0, 2.0), (2.0, 9.0)] {
                let r = Rect::from_min_size(g.p(x + 1.0, y + 1.0), egui::vec2(g.w(3.0), g.w(3.0)));
                painter.rect_filled(r, 0.0, color);
            }
            let pts = vec![
                g.p(1.5, 1.5),
                g.p(14.5, 1.5),
                g.p(14.5, 7.5),
                g.p(7.5, 7.5),
                g.p(7.5, 14.5),
                g.p(1.5, 14.5),
                g.p(1.5, 1.5),
            ];
            painter.extend(Shape::dashed_line(&pts, thin, g.w(1.6), g.w(1.4)));
            painter.circle_stroke(g.p(12.0, 12.0), g.w(2.0), thin);
        }
        Icon::Magnetic => {
            // A lasso loop with anchor points, and a small magnet.
            let loop_pts = ellipse_points(g.p(7.5, 7.0), g.w(5.5), g.w(4.5), 40);
            painter.add(Shape::line(loop_pts, thin));
            for (x, y) in [(2.0, 7.0), (7.5, 2.5), (13.0, 7.0)] {
                let r = Rect::from_center_size(g.p(x, y), egui::vec2(g.w(2.2), g.w(2.2)));
                painter.rect_filled(r, 0.0, color);
            }
            let u = quad_bezier(g.p(9.5, 11.0), g.p(12.0, 16.5), g.p(14.5, 11.0), 12);
            painter.add(Shape::line(u, Stroke::new(g.w(1.8), color)));
        }
        Icon::Transform => {
            line((8.0, 3.0), (8.0, 13.0), 1.25);
            line((3.0, 8.0), (13.0, 8.0), 1.25);
            let s = g.w(2.4);
            arrow_head(painter, g.p(8.0, 1.5), egui::vec2(0.0, -1.0), s, color);
            arrow_head(painter, g.p(8.0, 14.5), egui::vec2(0.0, 1.0), s, color);
            arrow_head(painter, g.p(1.5, 8.0), egui::vec2(-1.0, 0.0), s, color);
            arrow_head(painter, g.p(14.5, 8.0), egui::vec2(1.0, 0.0), s, color);
        }
        Icon::Eyedropper => {
            line((2.5, 13.5), (9.5, 6.5), 1.4);
            line((10.0, 6.0), (13.0, 3.0), 3.4);
            line((8.0, 4.5), (11.5, 8.0), 1.6);
            painter.circle_filled(g.p(2.5, 13.5), g.w(0.9), color);
        }
        Icon::Bucket => {
            // A tipped pail pouring a drop.
            let pail = vec![g.p(2.5, 7.0), g.p(8.0, 1.5), g.p(13.0, 6.5), g.p(7.5, 12.0)];
            painter.add(Shape::closed_line(pail, thin));
            line((2.5, 7.0), (13.0, 6.5), 1.2);
            painter.circle_filled(g.p(13.5, 11.5), g.w(1.6), color);
            line((13.5, 8.0), (13.5, 10.5), 1.2);
        }
        Icon::Liquify => {
            // A spiral: pixels pushed around.
            let pts: Vec<Pos2> = (0..=40)
                .map(|i| {
                    let t = i as f32 / 40.0;
                    let a = t * std::f32::consts::TAU * 1.75;
                    let r = 0.8 + t * 5.8;
                    g.p(8.0 + a.cos() * r, 8.0 + a.sin() * r)
                })
                .collect();
            painter.add(Shape::line(pts, thin));
        }
        Icon::Palette => {
            let outline = ellipse_points(g.p(8.0, 8.0), g.w(6.3), g.w(5.6), 32);
            painter.add(Shape::line(outline, thin));
            for (x, y) in [(5.0, 6.0), (8.0, 4.5), (11.0, 6.0), (11.0, 9.5)] {
                painter.circle_filled(g.p(x, y), g.w(1.1), color);
            }
            painter.circle_stroke(g.p(6.0, 10.5), g.w(1.3), thin);
        }
        Icon::Smudge => {
            // A fingertip dragging a streak.
            let tip = ellipse_points(g.p(10.5, 5.0), g.w(2.6), g.w(3.6), 20);
            painter.add(Shape::line(tip, thin));
            line((8.2, 7.5), (8.2, 14.5), 1.25);
            line((12.8, 7.5), (12.8, 14.5), 1.25);
            line((1.5, 12.5), (5.5, 9.0), 1.6);
            line((1.5, 9.5), (4.5, 6.5), 1.0);
        }
        Icon::Blur => {
            // A drop, softened by rings.
            let drop = vec![
                g.p(8.0, 1.8),
                g.p(11.8, 7.8),
                g.p(12.2, 10.3),
                g.p(10.6, 13.3),
                g.p(8.0, 14.3),
                g.p(5.4, 13.3),
                g.p(3.8, 10.3),
                g.p(4.2, 7.8),
            ];
            painter.add(Shape::closed_line(drop, thin));
            painter.circle_filled(g.p(8.0, 10.5), g.w(1.6), color.gamma_multiply(0.6));
        }
        Icon::Swap => {
            let curve = quad_bezier(g.p(3.5, 11.0), g.p(3.5, 3.5), g.p(11.0, 3.5), 16);
            painter.add(Shape::line(curve, thin));
            let s = g.w(2.6);
            arrow_head(painter, g.p(13.5, 3.5), egui::vec2(1.0, 0.0), s, color);
            arrow_head(painter, g.p(3.5, 13.5), egui::vec2(0.0, 1.0), s, color);
        }
        Icon::Eye | Icon::EyeOff => {
            let mut outline = quad_bezier(g.p(1.5, 8.0), g.p(8.0, 1.5), g.p(14.5, 8.0), 16);
            outline.extend(quad_bezier(
                g.p(14.5, 8.0),
                g.p(8.0, 14.5),
                g.p(1.5, 8.0),
                16,
            ));
            painter.add(Shape::line(outline, thin));
            painter.circle_filled(g.p(8.0, 8.0), g.w(2.2), color);
            if icon == Icon::EyeOff {
                line((2.5, 13.5), (13.5, 2.5), 1.4);
            }
        }
        Icon::Lock | Icon::Unlock => {
            painter.rect_filled(
                Rect::from_min_max(g.p(3.5, 7.5), g.p(12.5, 14.0)),
                0.0,
                color,
            );
            // Unlocked: the shackle is raised and its right leg is free.
            let lift = if icon == Icon::Lock { 0.0 } else { 2.0 };
            let top = 5.5 - lift;
            let arc = quad_bezier(g.p(5.5, top), g.p(8.0, top - 4.7), g.p(10.5, top), 12);
            painter.add(Shape::line(arc, thin));
            line((5.5, top), (5.5, 7.5), 1.25);
            if icon == Icon::Lock {
                line((10.5, top), (10.5, 7.5), 1.25);
            }
        }
        Icon::Trash => {
            line((2.5, 4.5), (13.5, 4.5), 1.25);
            painter.add(Shape::line(
                vec![g.p(6.0, 4.5), g.p(6.0, 2.5), g.p(10.0, 2.5), g.p(10.0, 4.5)],
                thin,
            ));
            painter.add(Shape::closed_line(
                vec![
                    g.p(4.0, 4.5),
                    g.p(12.0, 4.5),
                    g.p(11.2, 14.0),
                    g.p(4.8, 14.0),
                ],
                thin,
            ));
            line((6.8, 7.0), (6.8, 11.5), 1.0);
            line((9.2, 7.0), (9.2, 11.5), 1.0);
        }
        Icon::Plus => {
            line((8.0, 3.0), (8.0, 13.0), 1.5);
            line((3.0, 8.0), (13.0, 8.0), 1.5);
        }
        Icon::Undo | Icon::Redo => {
            // A hook arrow; redo is the mirror image of undo.
            let m = |x: f32| if icon == Icon::Undo { x } else { 16.0 - x };
            let mut arc = quad_bezier(g.p(m(4.5), 6.5), g.p(m(14.0), 5.0), g.p(m(12.5), 10.0), 12);
            arc.extend(quad_bezier(
                g.p(m(12.5), 10.0),
                g.p(m(11.0), 13.5),
                g.p(m(6.5), 13.0),
                8,
            ));
            painter.add(Shape::line(arc, Stroke::new(g.w(1.5), color)));
            let dir = if icon == Icon::Undo { -1.0 } else { 1.0 };
            arrow_head(
                painter,
                g.p(m(2.0), 6.5),
                egui::vec2(dir, 0.0),
                g.w(3.0),
                color,
            );
        }
        Icon::SelectBrush => {
            // Dashed blob with a brush tip: painting a selection.
            let pts = ellipse_points(g.p(7.0, 7.0), g.w(5.5), g.w(4.5), 40);
            painter.extend(Shape::dashed_line(&pts, thin, g.w(2.0), g.w(1.6)));
            line((14.0, 9.0), (10.0, 13.0), 1.6);
            painter.circle_filled(g.p(9.5, 13.5), g.w(1.6), color);
        }
        Icon::Folder => {
            // Folder with a tab.
            let body = vec![
                g.p(1.5, 4.0),
                g.p(6.0, 4.0),
                g.p(7.5, 5.5),
                g.p(14.5, 5.5),
                g.p(14.5, 13.0),
                g.p(1.5, 13.0),
            ];
            painter.add(Shape::closed_line(body, thin));
            line((1.5, 7.5), (14.5, 7.5), 1.0);
        }
        Icon::Mask => {
            // Square with a filled circle: the classic layer-mask glyph.
            painter.rect_stroke(
                Rect::from_min_max(g.p(1.5, 2.5), g.p(14.5, 13.5)),
                0.0,
                thin,
            );
            painter.circle_filled(g.p(8.0, 8.0), g.w(3.6), color);
        }
        Icon::Presets => {
            // A stack of preset tiles, each with a stroke.
            for (i, y) in [2.5_f32, 6.5, 10.5].into_iter().enumerate() {
                let tile = Rect::from_min_max(g.p(2.0, y), g.p(14.0, y + 3.2));
                if i == 0 {
                    painter.rect_filled(tile, 0.0, color);
                } else {
                    painter.rect_stroke(tile, 0.0, thin);
                }
            }
        }
        Icon::PanelLeft | Icon::PanelRight => {
            let outer = Rect::from_min_max(g.p(1.5, 2.5), g.p(14.5, 13.5));
            painter.rect_stroke(outer, 0.0, thin);
            let strip = if icon == Icon::PanelLeft {
                Rect::from_min_max(g.p(1.5, 2.5), g.p(6.0, 13.5))
            } else {
                Rect::from_min_max(g.p(10.0, 2.5), g.p(14.5, 13.5))
            };
            painter.rect_filled(strip, 0.0, color);
        }
        Icon::Menu => {
            for y in [4.0, 8.0, 12.0] {
                line((2.5, y), (13.5, y), 1.5);
            }
        }
        Icon::Finger => {
            // A fingertip touching down, with a ripple.
            let tip = Rect::from_min_max(g.p(6.0, 5.5), g.p(10.0, 15.5));
            painter.rect_stroke(tip, g.w(2.0), thin);
            painter.circle_filled(g.p(8.0, 5.5), g.w(1.3), color);
            let ripple = quad_bezier(g.p(3.0, 5.5), g.p(8.0, -1.5), g.p(13.0, 5.5), 12);
            painter.add(Shape::line(ripple, thin));
        }
    }
}
