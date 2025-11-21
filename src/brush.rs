use crate::canvas::{alpha_over, Canvas};
use crate::color::Color;
use crate::profiler::ScopeTimer;
use crate::vector::{distance, Vec2};
use rayon::{prelude::*, ThreadPool};

pub struct Brush {
    pub radius: f32,
    pub hardness: f32, // 0..1
    pub color: Color,
    pub spacing: f32, // multiple of radius
    mask: Option<BrushMask>,
    mode: DabMode,
}

impl Brush {
    pub fn new(radius: f32, hardness: f32, color: Color, spacing: f32) -> Self {
        Self {
            radius,
            hardness,
            color,
            spacing,
            mask: None,
            mode: DabMode::Masked,
        }
    }

    pub fn set_masked(&mut self, masked: bool) {
        self.mode = if masked { DabMode::Masked } else { DabMode::Naive };
        if !masked {
            self.mask = None;
        }
    }

    fn dab(&mut self, pool: &ThreadPool, canvas: &Canvas, center: Vec2) {
        match self.mode {
            DabMode::Masked => self.masked_dab(pool, canvas, center),
            DabMode::Naive => self.naive_dab(canvas, center),
        }
    }

    fn masked_dab(&mut self, pool: &ThreadPool, canvas: &Canvas, center: Vec2) {
        self.ensure_mask();
        let mask = self.mask.as_ref().unwrap();
        let tile_size = canvas.tile_size();

        let r_i32 = mask.radius.ceil() as i32;
        let min_x = (center.x.floor() as i32) - r_i32;
        let min_y = (center.y.floor() as i32) - r_i32;
        let max_x = min_x + mask.size as i32 - 1;
        let max_y = min_y + mask.size as i32 - 1;

        let canvas_w = canvas.width() as i32;
        let canvas_h = canvas.height() as i32;
        let base_color = self.color;

        let start_x = min_x.max(0) as usize;
        let start_y = min_y.max(0) as usize;
        let end_x = max_x.min(canvas_w - 1) as usize;
        let end_y = max_y.min(canvas_h - 1) as usize;

        if start_x > end_x || start_y > end_y {
            return;
        }

        let min_tx = start_x / tile_size;
        let max_tx = end_x / tile_size;
        let min_ty = start_y / tile_size;
        let max_ty = end_y / tile_size;

        let tiles: Vec<(usize, usize)> = (min_ty..=max_ty)
            .flat_map(|ty| (min_tx..=max_tx).map(move |tx| (tx, ty)))
            .collect();

        pool.install(|| {
            tiles.into_par_iter().for_each(|(tx, ty)| {
                if let Some(mut tile) = canvas.lock_tile(tx, ty) {
                    let data = match tile.data.as_mut() {
                        Some(d) => d,
                        None => return,
                    };

                    let tile_x0 = tx * tile_size;
                    let tile_y0 = ty * tile_size;
                    let overlap_min_x = start_x.max(tile_x0);
                    let overlap_max_x = end_x.min(tile_x0 + tile_size - 1);
                    let overlap_min_y = start_y.max(tile_y0);
                    let overlap_max_y = end_y.min(tile_y0 + tile_size - 1);

                    for gy in overlap_min_y..=overlap_max_y {
                        let mask_y = (gy as i32 - min_y) as usize;
                        let row_start = mask_y * mask.size;
                        let local_y = gy - tile_y0;

                        for gx in overlap_min_x..=overlap_max_x {
                            let mask_x = (gx as i32 - min_x) as usize;
                            let alpha_scale = mask.data[row_start + mask_x];
                            if alpha_scale == 0.0 {
                                continue;
                            }

                            let local_x = gx - tile_x0;
                            let idx = local_y * tile_size + local_x;
                            let dst = Color::from_color32(data[idx]);
                            let src = Color {
                                r: base_color.r,
                                g: base_color.g,
                                b: base_color.b,
                                a: base_color.a * alpha_scale,
                            };
                            let blended = alpha_over(src, dst);
                            data[idx] = blended.to_color32();
                        }
                    }
                }
            });
        });
    }

    fn naive_dab(&self, canvas: &Canvas, center: Vec2) {
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

    fn ensure_mask(&mut self) {
        let needs_rebuild = self.mask.as_ref().map_or(true, |m| {
            (m.radius - self.radius).abs() > f32::EPSILON
                || (m.hardness - self.hardness).abs() > f32::EPSILON
        });

        if needs_rebuild {
            self.mask = Some(BrushMask::build(self.radius, self.hardness));
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

    pub fn add_point(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        brush: &mut Brush,
        pos: Vec2,
    ) {
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
                brush.dab(pool, canvas, p);

                d += step;
                t = d / segment_len;
            }

            self.distance_since_last_dab = d - segment_len;
            if self.distance_since_last_dab < 0.0 {
                self.distance_since_last_dab = 0.0;
            }
        } else {
            // first point
            brush.dab(pool, canvas, pos);
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

struct BrushMask {
    size: usize,
    radius: f32,
    hardness: f32,
    data: Vec<f32>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DabMode {
    Masked,
    Naive,
}

impl BrushMask {
    fn build(radius: f32, hardness: f32) -> Self {
        let r_i32 = radius.ceil() as i32;
        let size = (r_i32 * 2 + 1) as usize;
        let mut data = vec![0.0; size * size];

        let softness = 0.05;
        let hardness = hardness.clamp(0.0, 1.0);
        let alpha = hardness + (1.0 - hardness) * softness;
        let r2 = radius * radius;

        for y in 0..size {
            let dy = y as i32 - r_i32;
            for x in 0..size {
                let dx = x as i32 - r_i32;
                let dist2 = (dx * dx + dy * dy) as f32;
                if dist2 <= r2 {
                    data[y * size + x] = alpha;
                }
            }
        }

        Self {
            size,
            radius,
            hardness,
            data,
        }
    }
}
