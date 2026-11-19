//! What the hand and body models share: the turned square of the picture
//! a model reads (its [`Roi`]), resampled into the model's input tensor.

use glam::Vec2;
use image::RgbImage;
use tract_onnx::prelude::*;

/// The square of the picture a model reads: its centre and side in
/// pixels, and which way is up in it (for a hand from the wrist towards
/// the middle finger, for a body from the hips towards the head), a unit
/// vector in the picture (y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Roi {
    pub center: Vec2,
    pub size: f32,
    pub up: Vec2,
}

impl Roi {
    /// The crop's right and down, in the picture.
    pub(super) fn axes(&self) -> (Vec2, Vec2) {
        let down = -self.up;
        (Vec2::new(-self.up.y, self.up.x), down)
    }

    /// Where a point of the crop (0..1 across and down) is in the picture.
    pub(super) fn to_image(self, u: f32, v: f32) -> Vec2 {
        let (right, down) = self.axes();
        self.center + right * (u - 0.5) * self.size + down * (v - 0.5) * self.size
    }

    /// The whole of a `w`×`h` picture, letterboxed into a square.
    pub(super) fn whole(w: f32, h: f32) -> Self {
        Self {
            center: Vec2::new(w, h) * 0.5,
            size: w.max(h),
            up: Vec2::NEG_Y,
        }
    }

    /// The square MediaPipe's alignment points give: centred on `center`,
    /// turned so `toward` is straight above it, twice as wide as they are
    /// apart, then grown by `scale`.
    pub(super) fn aligned(center: Vec2, toward: Vec2, scale: f32) -> Option<Self> {
        let up = (toward - center).try_normalize()?;
        let size = 2.0 * (toward - center).length() * scale;
        (size.is_finite() && size > 1.0).then_some(Self { center, size, up })
    }
}

/// How a model takes its picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layout {
    /// 1×side×side×3, RGB in 0..1 (the hand models, the body landmarks).
    Nhwc,
    /// 1×3×side×side, RGB in -1..1 (the body detector).
    NchwSigned,
}

pub(super) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x.clamp(-80.0, 80.0)).exp())
}

/// `roi` of `image` resampled (bilinear) to a `side`-pixel square, as
/// `layout` says, black outside the picture.
pub(super) fn crop(image: &RgbImage, roi: &Roi, side: usize, layout: Layout) -> Tensor {
    let (w, h) = (image.width() as i64, image.height() as i64);
    let raw = image.as_raw();
    let texel = |x: i64, y: i64| -> [f32; 3] {
        if x < 0 || y < 0 || x >= w || y >= h {
            return [0.0; 3];
        }
        let i = ((y * w + x) * 3) as usize;
        [raw[i] as f32, raw[i + 1] as f32, raw[i + 2] as f32]
    };
    let (right, down) = roi.axes();
    let step = roi.size / side as f32;
    let origin = roi.center - (right + down) * roi.size * 0.5;
    let mut data = vec![0f32; side * side * 3];
    // A crop much larger than its square would alias: average a few
    // samples a pixel then.
    let taps = (step / 1.5).ceil().clamp(1.0, 4.0) as usize;
    let inv = 1.0 / (255.0 * (taps * taps) as f32);
    let (scale, offset) = match layout {
        Layout::Nhwc => (1.0, 0.0),
        Layout::NchwSigned => (2.0, -1.0),
    };
    for v in 0..side {
        for u in 0..side {
            let mut sum = [0f32; 3];
            for ty in 0..taps {
                for tx in 0..taps {
                    let fu = u as f32 + (tx as f32 + 0.5) / taps as f32;
                    let fv = v as f32 + (ty as f32 + 0.5) / taps as f32;
                    let p = origin + right * fu * step + down * fv * step - Vec2::splat(0.5);
                    let (x0, y0) = (p.x.floor(), p.y.floor());
                    let (fx, fy) = (p.x - x0, p.y - y0);
                    let (x0, y0) = (x0 as i64, y0 as i64);
                    let a = texel(x0, y0);
                    let b = texel(x0 + 1, y0);
                    let c = texel(x0, y0 + 1);
                    let d = texel(x0 + 1, y0 + 1);
                    for k in 0..3 {
                        let top = a[k] + (b[k] - a[k]) * fx;
                        let bottom = c[k] + (d[k] - c[k]) * fx;
                        sum[k] += top + (bottom - top) * fy;
                    }
                }
            }
            for (k, total) in sum.into_iter().enumerate() {
                let o = match layout {
                    Layout::Nhwc => (v * side + u) * 3 + k,
                    Layout::NchwSigned => k * side * side + v * side + u,
                };
                data[o] = total * inv * scale + offset;
            }
        }
    }
    let shape = match layout {
        Layout::Nhwc => [1, side, side, 3],
        Layout::NchwSigned => [1, 3, side, side],
    };
    Tensor::from_shape(&shape, &data).expect("the crop's shape fits its data")
}
