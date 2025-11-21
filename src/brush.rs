use crate::canvas::Canvas;
use crate::color::Color;
use crate::profiler::ScopeTimer;
use crate::vector::{distance, Vec2};

pub struct Brush {
    pub radius: f32,
    pub hardness: f32, // 0..1
    pub color: Color,
    pub spacing: f32, // multiple of radius
}

impl Brush {
    fn dab(&self, canvas: &mut Canvas, center: Vec2) {
        let r = self.radius;
        let r_i32 = r.ceil() as i32;

        let min_x = (center.x.floor() as i32) - r_i32;
        let max_x = (center.x.floor() as i32) + r_i32;
        let min_y = (center.y.floor() as i32) - r_i32;
        let max_y = (center.y.floor() as i32) + r_i32;

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let dx = x as f32 + 0.5 - center.x;
                let dy = y as f32 + 0.5 - center.y;
                let dist = (dx * dx + dy * dy).sqrt();

                if dist > r {
                    continue;
                }

                let softness = 0.05;
                let hardness = self.hardness.clamp(0.0, 1.0);
                let alpha = hardness + (1.0 - hardness) * softness;

                let src = Color {
                    r: self.color.r,
                    g: self.color.g,
                    b: self.color.b,
                    a: self.color.a * alpha,
                };

                canvas.blend_pixel(x, y, src);
            }
        }
    }
}

pub struct StrokeState {
    pub last_pos: Option<Vec2>,
    distance_since_last_dab: f32,
    stroke_timer: Option<ScopeTimer>,
}

impl StrokeState {
    pub fn new() -> Self {
        Self {
            last_pos: None,
            distance_since_last_dab: 0.0,
            stroke_timer: Some(ScopeTimer::new("stroke")),
        }
    }

    pub fn add_point(&mut self, canvas: &mut Canvas, brush: &Brush, pos: Vec2) {
        let step = brush.radius * brush.spacing.max(0.01);

        if let Some(prev) = self.last_pos {
            let segment_len = distance(prev, pos);

            if segment_len == 0.0 {
                return;
            }

            let mut d = self.distance_since_last_dab;
            let mut t = d / segment_len;

            while t <= 1.0 {
                let x = prev.x + (pos.x - prev.x) * t;
                let y = prev.y + (pos.y - prev.y) * t;
                let p = Vec2 { x, y };
                brush.dab(canvas, p);

                d += step;
                t = d / segment_len;
            }

            self.distance_since_last_dab = d - segment_len;
            if self.distance_since_last_dab < 0.0 {
                self.distance_since_last_dab = 0.0;
            }
        } else {
            // first point
            brush.dab(canvas, pos);
            self.distance_since_last_dab = 0.0;
        }

        self.last_pos = Some(pos);
    }

    pub fn end(&mut self) {
        self.last_pos = None;
        self.distance_since_last_dab = 0.0;
        // Drop the timer so stroke-level duration is reported when the stroke ends.
        self.stroke_timer.take();
    }
}
