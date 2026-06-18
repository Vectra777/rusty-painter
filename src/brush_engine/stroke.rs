use crate::brush_engine::brush::{Brush, StabilizerAlgorithm};
use crate::canvas::Canvas;
use crate::canvas::history::UndoAction;
use crate::selection::SelectionManager;
use eframe::egui::Vec2;
use rand::Rng;
use rayon::ThreadPool;
use std::collections::HashSet;

/// Shared drawing dependencies for adding points to a stroke.
pub struct StrokeContext<'a> {
    pool: &'a ThreadPool,
    canvas: &'a Canvas,
    selection: Option<&'a SelectionManager>,
    undo_action: &'a mut UndoAction,
    modified_tiles: &'a mut HashSet<(usize, usize)>,
}

impl<'a> StrokeContext<'a> {
    pub fn new(
        pool: &'a ThreadPool,
        canvas: &'a Canvas,
        selection: Option<&'a SelectionManager>,
        undo_action: &'a mut UndoAction,
        modified_tiles: &'a mut HashSet<(usize, usize)>,
    ) -> Self {
        Self {
            pool,
            canvas,
            selection,
            undo_action,
            modified_tiles,
        }
    }

    fn dab(&mut self, brush: &mut Brush, pos: Vec2) {
        brush.dab(
            self.pool,
            self.canvas,
            self.selection,
            pos,
            self.undo_action,
            self.modified_tiles,
        );
    }
}

/// Tracks per-stroke state like the last position and spacing accumulator.
pub struct StrokeState {
    pub last_pos: Option<Vec2>,
    pub velocity: Vec2,
    dist_until_next_blit: f32,
}

impl StrokeState {
    /// Create an empty stroke state.
    pub fn new() -> Self {
        Self {
            last_pos: None,
            velocity: Vec2 { x: 0.0, y: 0.0 },
            dist_until_next_blit: 0.0,
        }
    }

    /// Add a new sample to the stroke, interpolating dabs based on spacing and jitter.
    pub fn add_point(&mut self, brush: &mut Brush, raw_pos: Vec2, context: &mut StrokeContext<'_>) {
        if brush.pixel_perfect {
            self.add_point_pixel_perfect(brush, raw_pos, context);
            return;
        }

        let pos = match brush.stabilizer_algorithm {
            StabilizerAlgorithm::None => raw_pos,
            StabilizerAlgorithm::Simple => {
                if brush.stabilizer > 0.0 {
                    if let Some(prev) = self.last_pos {
                        let factor = 1.0 - (brush.stabilizer * 0.95);
                        let diff = raw_pos - prev;
                        prev + diff * factor
                    } else {
                        raw_pos
                    }
                } else {
                    raw_pos
                }
            }
            StabilizerAlgorithm::Dynamic => {
                if let Some(prev) = self.last_pos {
                    // Physics simulation:
                    // Treat the cursor as a target pulling the brush with a spring.
                    // Force = (Target - Current)
                    // Acceleration = Force / Mass
                    // Velocity += Acceleration
                    // Velocity *= (1.0 - Drag)
                    // NewPos = Current + Velocity

                    let force = raw_pos - prev;
                    // Scale mass so 0.01..1.0 maps to useful behavior
                    let mass = brush.stabilizer_mass.max(0.01) * 50.0;
                    let acceleration = force / mass;

                    self.velocity += acceleration;
                    // Drag 0.0..1.0
                    self.velocity *= 1.0 - brush.stabilizer_drag;

                    prev + self.velocity
                } else {
                    raw_pos
                }
            }
        };

        let spacing_dist = (brush.brush_options.spacing / 100.0) * brush.brush_options.diameter;
        let spacing_dist = spacing_dist.max(0.5); // Avoid infinite loops

        if let Some(prev) = self.last_pos {
            let delta = pos - prev;
            let mut dist_left = delta.length();

            if dist_left == 0.0 {
                return;
            }

            let unit_step = delta / dist_left;
            let mut cur_pos = prev;

            while dist_left >= self.dist_until_next_blit {
                // Take a step to the next blit point.
                cur_pos += unit_step * self.dist_until_next_blit;
                dist_left -= self.dist_until_next_blit;

                // Blit.
                let mut p = cur_pos;
                if brush.jitter > 0.0 {
                    let mut rng = rand::rng();
                    let jitter_amount = (brush.jitter / 100.0) * brush.brush_options.diameter;
                    let jx = rng.random_range(-jitter_amount..=jitter_amount);
                    let jy = rng.random_range(-jitter_amount..=jitter_amount);
                    p.x += jx;
                    p.y += jy;
                }
                context.dab(brush, p);

                self.dist_until_next_blit = spacing_dist;
            }

            // Take the partial step to land at the sample.
            self.dist_until_next_blit -= dist_left;
        } else {
            // first point
            let mut p = pos;
            if brush.jitter > 0.0 {
                let mut rng = rand::rng();
                let jx = rng.random_range(-brush.jitter..=brush.jitter);
                let jy = rng.random_range(-brush.jitter..=brush.jitter);
                p.x += jx;
                p.y += jy;
            }
            context.dab(brush, p);
            self.dist_until_next_blit = spacing_dist;
        }

        self.last_pos = Some(pos);
    }

    /// Pixel-perfect Bresenham line stepping to avoid gaps when snapping to pixels.
    fn add_point_pixel_perfect(
        &mut self,
        brush: &mut Brush,
        pos: Vec2,
        context: &mut StrokeContext<'_>,
    ) {
        let x1 = pos.x.floor() as i32;
        let y1 = pos.y.floor() as i32;

        if let Some(prev) = self.last_pos {
            let x0 = prev.x.floor() as i32;
            let y0 = prev.y.floor() as i32;

            if x0 == x1 && y0 == y1 {
                return;
            }

            let dx = (x1 - x0).abs();
            let dy = -(y1 - y0).abs();
            let sx = if x0 < x1 { 1 } else { -1 };
            let sy = if y0 < y1 { 1 } else { -1 };
            let mut err = dx + dy;

            let mut x = x0;
            let mut y = y0;

            loop {
                context.dab(
                    brush,
                    Vec2 {
                        x: x as f32 + 0.5,
                        y: y as f32 + 0.5,
                    },
                );

                if x == x1 && y == y1 {
                    break;
                }
                let e2 = 2 * err;
                if e2 >= dy {
                    err += dy;
                    x += sx;
                }
                if e2 <= dx {
                    err += dx;
                    y += sy;
                }
            }
        } else {
            context.dab(
                brush,
                Vec2 {
                    x: x1 as f32 + 0.5,
                    y: y1 as f32 + 0.5,
                },
            );
        }
        self.last_pos = Some(pos);
    }

    /// Reset the stroke state.
    pub fn end(&mut self) {
        self.last_pos = None;
        self.dist_until_next_blit = 0.0;
    }
}

impl Default for StrokeState {
    fn default() -> Self {
        Self::new()
    }
}
