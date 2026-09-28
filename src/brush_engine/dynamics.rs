//! Brush dynamics: what changes from dab to dab along a stroke, besides
//! pen pressure. The tip's angle and squash, tapers at the ends, stroke
//! speed, and randomness. Everything defaults to off, and a brush with all
//! of it off paints exactly as before (the per-dab path isn't even taken).
//!
//! [`DabVar`] is what one dab ends up with: a size factor, a strength
//! factor and the tip's orientation (rotation and squash as a matrix from
//! canvas offsets to tip offsets).

use eframe::egui::Vec2;

/// The tip's angle and shape.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TipShape {
    /// Degrees, counter-clockwise; with `follow_stroke`, added to the
    /// stroke's direction.
    pub angle: f32,
    /// Turn the tip with the direction the stroke is going (calligraphy,
    /// textured brushes).
    pub follow_stroke: bool,
    /// Random turn of each dab, up to this many degrees either way.
    pub random_angle: f32,
    /// Squash: height over width (1 = round, 0.1 = a thin nib).
    pub ratio: f32,
    /// Turn the tip the way the pen leans (tablets that report tilt), on
    /// top of `angle`.
    pub follow_tilt: bool,
}

impl Default for TipShape {
    fn default() -> Self {
        Self {
            angle: 0.0,
            follow_stroke: false,
            random_angle: 0.0,
            ratio: 1.0,
            follow_tilt: false,
        }
    }
}

impl TipShape {
    pub fn is_active(&self) -> bool {
        self.angle != 0.0
            || self.follow_stroke
            || self.follow_tilt
            || self.random_angle > 0.0
            || self.ratio < 1.0
    }
}

/// Thinning (and/or fading) the ends of a stroke.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Taper {
    /// Length of the taper at the start, in canvas pixels (0 = none).
    pub start: f32,
    /// Length of the taper at the end, in canvas pixels (0 = none). The last
    /// stretch is kept redrawable while drawing, so it thins when the pen
    /// lifts without the line lagging behind the pen.
    pub end: f32,
    /// The taper narrows the tip.
    pub size: bool,
    /// The taper fades the paint.
    pub opacity: bool,
    /// Size / opacity at the very tip of the taper (0..1).
    pub min: f32,
}

impl Default for Taper {
    fn default() -> Self {
        Self {
            start: 0.0,
            end: 0.0,
            size: true,
            opacity: false,
            min: 0.0,
        }
    }
}

impl Taper {
    pub fn is_active(&self) -> bool {
        (self.start > 0.0 || self.end > 0.0) && (self.size || self.opacity)
    }

    /// The factor at `along` pixels from the taper's far end (0 = the tip,
    /// `length` and beyond = full), eased so the point is fine but not
    /// needle-thin for long.
    pub fn factor(&self, along: f32, length: f32) -> f32 {
        if length <= 0.0 {
            return 1.0;
        }
        let t = (along / length).clamp(0.0, 1.0);
        // Ease out: most of the thinning happens near the very end.
        let eased = 1.0 - (1.0 - t) * (1.0 - t);
        self.min + (1.0 - self.min) * eased
    }
}

/// What stroke speed changes.
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SpeedDynamics {
    /// -1..1: negative thins fast strokes (ink), positive thickens them.
    pub size: f32,
    /// -1..1: negative fades fast strokes (dry brush), positive darkens.
    pub opacity: f32,
}

impl SpeedDynamics {
    pub fn is_active(&self) -> bool {
        self.size != 0.0 || self.opacity != 0.0
    }
}

/// What pen tilt changes (tablets that report it).
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TiltDynamics {
    /// -1..1: positive widens the stroke as the pen leans (pencil on its
    /// side), negative narrows it.
    pub size: f32,
    /// -1..1: positive strengthens the paint as the pen leans, negative
    /// fades it.
    pub opacity: f32,
}

impl TiltDynamics {
    pub fn is_active(&self) -> bool {
        self.size != 0.0 || self.opacity != 0.0
    }
}

/// How the pen leans, in canvas terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenTilt {
    /// 0 upright .. 1 flat.
    pub lean: f32,
    /// The way it leans, radians on the canvas (counter-clockwise, y down).
    pub direction: f32,
}

/// Screen speed (points per second) counted as "fast": the full effect.
pub const FAST_SPEED: f32 = 2500.0;

/// Per-dab randomness.
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Randomness {
    /// Size randomness, 0..1 (1: a dab can shrink to nothing).
    pub size: f32,
    /// Opacity randomness, 0..1.
    pub opacity: f32,
    /// Dabs per step (spray / particles); spread by the brush's scatter.
    pub count: u32,
    /// Hue randomness in degrees either way (0..180).
    pub hue: f32,
    /// Saturation randomness, 0..1.
    pub saturation: f32,
    /// Value (lightness) randomness, 0..1.
    pub value: f32,
}

impl Randomness {
    pub fn is_active(&self) -> bool {
        self.size > 0.0 || self.opacity > 0.0 || self.count > 1 || self.has_color()
    }

    pub fn has_color(&self) -> bool {
        self.hue > 0.0 || self.saturation > 0.0 || self.value > 0.0
    }

    /// Dabs placed per step.
    pub fn dabs_per_step(&self) -> u32 {
        self.count.clamp(1, 64)
    }
}

/// All of a brush's dynamics.
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BrushDynamics {
    pub tip: TipShape,
    pub taper: Taper,
    pub speed: SpeedDynamics,
    pub random: Randomness,
    pub tilt: TiltDynamics,
}

impl BrushDynamics {
    /// Whether any dab can differ from the plain brush (size, strength,
    /// orientation, colour).
    pub fn is_active(&self) -> bool {
        self.tip.is_active()
            || self.taper.is_active()
            || self.speed.is_active()
            || self.random.is_active()
            || self.tilt.is_active()
    }
}

/// How one dab differs from the plain brush.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DabVar {
    /// Size factor (1 = the brush size at this pressure).
    pub scale: f32,
    /// Strength factor (flow × opacity), 0..1.
    pub strength: f32,
    /// Canvas offset from the centre → tip offset (rotation and squash),
    /// row-major.
    pub orient: [f32; 4],
    /// Colour shift: hue (degrees), saturation and value offsets.
    pub hsv: [f32; 3],
}

impl Default for DabVar {
    fn default() -> Self {
        Self {
            scale: 1.0,
            strength: 1.0,
            orient: IDENTITY,
            hsv: [0.0; 3],
        }
    }
}

pub const IDENTITY: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

/// The matrix taking canvas offsets to a tip turned by `angle` radians
/// (counter-clockwise on screen, where y points down) and squashed to
/// `ratio`: the tip's x axis along `angle`, its y axis shortened.
pub fn tip_orientation(angle: f32, ratio: f32) -> [f32; 4] {
    let (s, c) = angle.sin_cos();
    let inv = 1.0 / ratio.clamp(0.02, 1.0);
    // Rotate by -angle (screen y down: +angle turns counter-clockwise),
    // then stretch tip y so the squashed axis is shorter on canvas.
    [c, -s, s * inv, c * inv]
}

/// `a` then `b`: the matrix for applying `b` first, then `a`.
pub fn compose(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
    ]
}

/// `color` (unmultiplied sRGB) with its hue turned by `hsv[0]` degrees and
/// its saturation and value moved by `hsv[1]`, `hsv[2]`; sRGB 0..1.
pub fn shift_hsv(color: eframe::egui::Color32, hsv: [f32; 3]) -> [f32; 3] {
    let [r, g, b, _] = color.to_srgba_unmultiplied().map(|v| v as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let mut h = if d <= 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let mut s = if max <= 0.0 { 0.0 } else { d / max };
    let mut v = max;
    h = (h + hsv[0]).rem_euclid(360.0);
    s = (s + hsv[1]).clamp(0.0, 1.0);
    v = (v + hsv[2]).clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// Direction of travel from `from` to `to`, in radians on screen
/// (counter-clockwise, y down), or `None` if they're the same point.
pub fn direction(from: Vec2, to: Vec2) -> Option<f32> {
    let d = to - from;
    (d.length_sq() > 1e-8).then(|| (-d.y).atan2(d.x))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: [f32; 4], v: (f32, f32)) -> (f32, f32) {
        (m[0] * v.0 + m[1] * v.1, m[2] * v.0 + m[3] * v.1)
    }

    #[test]
    fn a_turned_squashed_tip_maps_its_axes() {
        // Turned 90°: the tip's x axis points up the screen (canvas -y).
        let m = tip_orientation(std::f32::consts::FRAC_PI_2, 0.5);
        let (x, y) = apply(m, (0.0, -10.0));
        assert!((x - 10.0).abs() < 1e-4 && y.abs() < 1e-4, "{x} {y}");
        // Across the tip, distances count double (squashed to half).
        let (x, y) = apply(m, (10.0, 0.0));
        assert!(x.abs() < 1e-4 && (y.abs() - 20.0).abs() < 1e-4, "{x} {y}");
    }

    #[test]
    fn tapers_ease_from_the_minimum_to_full() {
        let taper = Taper {
            min: 0.2,
            ..Default::default()
        };
        assert_eq!(taper.factor(0.0, 50.0), 0.2);
        assert_eq!(taper.factor(50.0, 50.0), 1.0);
        assert_eq!(taper.factor(80.0, 50.0), 1.0);
        let mid = taper.factor(25.0, 50.0);
        assert!(mid > 0.6 && mid < 1.0, "eased: {mid}");
        assert_eq!(taper.factor(10.0, 0.0), 1.0, "no taper");
    }

    #[test]
    fn nothing_is_active_by_default() {
        assert!(!BrushDynamics::default().is_active());
        assert_eq!(DabVar::default().orient, IDENTITY);
    }

    #[test]
    fn directions_follow_the_screen() {
        let right = direction(Vec2::ZERO, Vec2::new(5.0, 0.0)).unwrap();
        let up = direction(Vec2::ZERO, Vec2::new(0.0, -5.0)).unwrap();
        assert!(right.abs() < 1e-6);
        assert!((up - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert!(direction(Vec2::ZERO, Vec2::ZERO).is_none());
    }
}
