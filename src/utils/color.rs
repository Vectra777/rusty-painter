use eframe::egui::Color32;

/// Simple RGBA color stored as linear floats in 0..1.
#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// Construct from 0-255 channel values.
    pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// Solid white convenience color.
    pub fn white() -> Self {
        Self::rgba(255, 255, 255, 255)
    }

    /// Convert HSVA values (0..1) into an RGBA color.
    pub fn from_hsva(h: f32, s: f32, v: f32, a: f32) -> Self {
        // h is wrapped into [0,1) so callers can pass any float
        let h = ((h % 1.0) + 1.0) % 1.0;
        let s = s.clamp(0.0, 1.0);
        let v = v.clamp(0.0, 1.0);
        let a = a.clamp(0.0, 1.0);

        let c = v * s;
        let x = c * (1.0 - (((h * 6.0) % 2.0) - 1.0).abs());
        let m = v - c;

        let (r1, g1, b1) = match (h * 6.0).floor() as i32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };

        Self {
            r: r1 + m,
            g: g1 + m,
            b: b1 + m,
            a,
        }
    }

    /// Convert RGBA into HSVA for UI sliders.
    pub fn to_hsva(&self) -> (f32, f32, f32, f32) {
        let r = self.r;
        let g = self.g;
        let b = self.b;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;

        let mut h = if delta == 0.0 {
            0.0
        } else if max == r {
            ((g - b) / delta) % 6.0
        } else if max == g {
            ((b - r) / delta) + 2.0
        } else {
            ((r - g) / delta) + 4.0
        };

        h /= 6.0;
        if h < 0.0 {
            h += 1.0;
        }

        let s = if max == 0.0 { 0.0 } else { delta / max };
        let v = max;
        let a = self.a;
        (h, s, v, a)
    }

    /// Convert to egui's 8-bit color format.
    pub fn to_color32(&self) -> Color32 {
        Color32::from_rgba_unmultiplied(
            (self.r * 255.0) as u8,
            (self.g * 255.0) as u8,
            (self.b * 255.0) as u8,
            (self.a * 255.0) as u8,
        )
    }

    /// Convert from egui's 8-bit color format to linear floats.
    pub fn from_color32(c: Color32) -> Self {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }
}
