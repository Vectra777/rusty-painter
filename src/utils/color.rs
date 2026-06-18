use eframe::egui::Color32;

pub type Color = Color32;

pub trait ColorManipulation {
    fn from_hsva(h: f32, s: f32, v: f32, a: f32) -> Self;
    fn to_hsva(self) -> (f32, f32, f32, f32);

    fn from_gray_alpha(value: f32, a: f32) -> Self;
    fn to_color32(self) -> Color32;
}

fn clamp_to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

impl ColorManipulation for Color32 {
    fn from_hsva(h: f32, s: f32, v: f32, a: f32) -> Self {
        let h = h.rem_euclid(1.0);
        let s = s.clamp(0.0, 1.0);
        let v = v.clamp(0.0, 1.0);
        let a = a.clamp(0.0, 1.0);

        let i = (h * 6.0).floor();
        let f = h * 6.0 - i;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * f);
        let t = v * (1.0 - s * (1.0 - f));

        let (r, g, b) = match (i as i32) % 6 {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };

        Color32::from_rgba_unmultiplied(
            clamp_to_u8(r),
            clamp_to_u8(g),
            clamp_to_u8(b),
            clamp_to_u8(a),
        )
    }

    fn to_hsva(self) -> (f32, f32, f32, f32) {
        let [r, g, b, a] = self.to_srgba_unmultiplied();
        let r = r as f32 / 255.0;
        let g = g as f32 / 255.0;
        let b = b as f32 / 255.0;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;

        let h = if delta <= f32::EPSILON {
            0.0
        } else if (max - r).abs() <= f32::EPSILON {
            ((g - b) / delta).rem_euclid(6.0) / 6.0
        } else if (max - g).abs() <= f32::EPSILON {
            ((b - r) / delta + 2.0) / 6.0
        } else {
            ((r - g) / delta + 4.0) / 6.0
        };

        let s = if max <= f32::EPSILON {
            0.0
        } else {
            delta / max
        };
        let v = max;

        (h, s, v, a as f32 / 255.0)
    }

    fn from_gray_alpha(value: f32, a: f32) -> Self {
        let v = clamp_to_u8(value);
        Color32::from_rgba_unmultiplied(v, v, v, clamp_to_u8(a))
    }

    fn to_color32(self) -> Color32 {
        self
    }
}
