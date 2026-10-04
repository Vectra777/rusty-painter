//! Brush dynamics: what changes from dab to dab along a stroke, besides
//! pen pressure. The tip's angle and squash, tapers at the ends, stroke
//! speed, and randomness. Everything defaults to off, and a brush with all
//! of it off paints exactly as before (the per-dab path isn't even taken).
//!
//! [`DabVar`] is what one dab ends up with: a size factor, a strength
//! factor and the tip's orientation (rotation and squash as a matrix from
//! canvas offsets to tip offsets).

use crate::canvas::blend::Unmultiply;
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
    /// Turn the tip with the pen's barrel rotation (pens that report it,
    /// like Wacom's Art Pen), on top of `angle`.
    pub follow_barrel: bool,
    /// Mirror dabs across the tip's length (left-right) or its width
    /// (top-bottom): at random, half the dabs each
    /// way; or, with an input mapped to [`DabSetting::Mirror`], the dabs
    /// whose input reaches half.
    pub random_flip_x: bool,
    pub random_flip_y: bool,
}

impl Default for TipShape {
    fn default() -> Self {
        Self {
            angle: 0.0,
            follow_stroke: false,
            random_angle: 0.0,
            ratio: 1.0,
            follow_tilt: false,
            follow_barrel: false,
            random_flip_x: false,
            random_flip_y: false,
        }
    }
}

impl TipShape {
    pub fn is_active(&self) -> bool {
        self.angle != 0.0
            || self.follow_stroke
            || self.follow_tilt
            || self.follow_barrel
            || self.random_angle > 0.0
            || self.ratio < 1.0
            || self.random_flip_x
            || self.random_flip_y
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

/// The pen's barrel rotation and finger wheel, where the tablet reports
/// them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PenBarrel {
    /// Turn about the pen's own axis, radians on the canvas
    /// (counter-clockwise, y down).
    pub rotation: Option<f32>,
    /// An airbrush pen's finger wheel (tangential pressure), 0..1.
    pub wheel: Option<f32>,
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

/// An input a brush setting can follow (see [`InputMapping`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Sensor {
    Pressure,
    /// Stroke speed (0 still, 1 at [`FAST_SPEED`] and above).
    Speed,
    /// How far the pen leans (0 upright, 1 flat).
    Tilt,
    /// Which way the pen leans, once round the circle.
    TiltDirection,
    /// Which way the stroke is going, once round the circle.
    Direction,
    /// Distance along the stroke, up to the mapping's length (px).
    Distance,
    /// Time since the stroke started, up to the mapping's length (s).
    Time,
    /// A new random value for each dab.
    RandomDab,
    /// One random value for the whole stroke.
    RandomStroke,
    /// The pen's barrel rotation, once round the circle.
    Rotation,
    /// An airbrush pen's finger wheel.
    Wheel,
    /// The highest pressure so far in the stroke: it never eases off.
    PressureIn,
    /// Dabs since the stroke started, up to the mapping's length (in
    /// dabs).
    Fade,
    /// Where the dab is on an enabled perspective assistant: 0 on its
    /// horizon, 1 at its corner farthest from it (and anywhere off the
    /// assistants).
    Perspective,
    /// How far the pen leans left (0) or right (1), upright in the middle.
    XTilt,
    /// How far the pen leans away (0) or toward you (1), upright in the
    /// middle.
    YTilt,
}

impl Sensor {
    pub const ALL: [Sensor; 16] = [
        Sensor::Pressure,
        Sensor::Speed,
        Sensor::Tilt,
        Sensor::TiltDirection,
        Sensor::Direction,
        Sensor::Distance,
        Sensor::Time,
        Sensor::RandomDab,
        Sensor::RandomStroke,
        Sensor::Rotation,
        Sensor::Wheel,
        Sensor::PressureIn,
        Sensor::Fade,
        Sensor::Perspective,
        Sensor::XTilt,
        Sensor::YTilt,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Sensor::Pressure => "Pressure",
            Sensor::Speed => "Speed",
            Sensor::Tilt => "Tilt",
            Sensor::TiltDirection => "Tilt direction",
            Sensor::Direction => "Stroke direction",
            Sensor::Distance => "Distance",
            Sensor::Time => "Time",
            Sensor::RandomDab => "Random (each dab)",
            Sensor::RandomStroke => "Random (each stroke)",
            Sensor::Rotation => "Barrel rotation",
            Sensor::Wheel => "Airbrush wheel",
            Sensor::PressureIn => "Pressure in (highest so far)",
            Sensor::Fade => "Fade (dabs)",
            Sensor::Perspective => "Perspective",
            Sensor::XTilt => "X tilt",
            Sensor::YTilt => "Y tilt",
        }
    }

    /// Whether the mapping's length applies (distance in px, time in s,
    /// fade in dabs).
    pub fn has_length(self) -> bool {
        matches!(self, Sensor::Distance | Sensor::Time | Sensor::Fade)
    }
}

/// A setting an input can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DabSetting {
    Size,
    Opacity,
    /// Turns the tip, up to 180° at full amount.
    Angle,
    /// Flattens the tip.
    Squash,
    /// Turns the hue, up to 180° at full amount.
    Hue,
    Saturation,
    Value,
    /// Scales the paper texture's strength.
    TextureStrength,
    /// Hardens the tip's edge (softens it the other way), up to 100%.
    Hardness,
    /// Scatters the dab, up to a brush width at full amount.
    Scatter,
    /// Mixes the brush colour with the secondary colour, all the way at
    /// full amount.
    ColorMix,
    /// Scales the hard edges' strength (`Brush::sharpness`), like size.
    Sharpness,
    /// Darkens the colour toward black, like size
    /// scales the dab.
    Darken,
    /// Softens a round or square tip, like size scales the dab, down to a
    /// tenth: a smaller solid core, or a
    /// softness curve's inner points lowered.
    Softness,
    /// Mirrors the tip (the ways set in the tip's mirror options) when it
    /// reaches half.
    Mirror,
    /// A sketch brush's density, line width and line offset, each like
    /// size scales the dab.
    SketchDensity,
    SketchWidth,
    SketchOffset,
    /// Scales the flow, like size scales the dab.
    Flow,
    /// Scales the spacing between dabs (read where each sample comes in).
    Spacing,
    /// How strongly a lightness-mapped tip's picture lightens and darkens
    /// the colour (0: the plain colour).
    Lightness,
}

impl DabSetting {
    pub const ALL: [DabSetting; 21] = [
        DabSetting::Size,
        DabSetting::Opacity,
        DabSetting::Angle,
        DabSetting::Squash,
        DabSetting::Hue,
        DabSetting::Saturation,
        DabSetting::Value,
        DabSetting::TextureStrength,
        DabSetting::Hardness,
        DabSetting::Scatter,
        DabSetting::ColorMix,
        DabSetting::Sharpness,
        DabSetting::Darken,
        DabSetting::Softness,
        DabSetting::Mirror,
        DabSetting::SketchDensity,
        DabSetting::SketchWidth,
        DabSetting::SketchOffset,
        DabSetting::Flow,
        DabSetting::Spacing,
        DabSetting::Lightness,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DabSetting::Size => "Size",
            DabSetting::Opacity => "Opacity",
            DabSetting::Angle => "Angle",
            DabSetting::Squash => "Squash",
            DabSetting::Hue => "Hue",
            DabSetting::Saturation => "Saturation",
            DabSetting::Value => "Value",
            DabSetting::TextureStrength => "Texture strength",
            DabSetting::Hardness => "Hardness",
            DabSetting::Scatter => "Scatter",
            DabSetting::ColorMix => "Secondary colour mix",
            DabSetting::Sharpness => "Hard edges",
            DabSetting::Darken => "Darken",
            DabSetting::Softness => "Softness",
            DabSetting::Mirror => "Mirror",
            DabSetting::SketchDensity => "Sketch density",
            DabSetting::SketchWidth => "Sketch line width",
            DabSetting::SketchOffset => "Sketch line offset",
            DabSetting::Flow => "Flow",
            DabSetting::Spacing => "Spacing",
            DabSetting::Lightness => "Lightness strength",
        }
    }

    /// The input adds to it (rather than scaling it), so it can swing it
    /// both ways (see [`InputMapping::both_ways`]).
    pub fn adds(self) -> bool {
        matches!(
            self,
            DabSetting::Angle
                | DabSetting::Hue
                | DabSetting::Saturation
                | DabSetting::Value
                | DabSetting::Hardness
                | DabSetting::Scatter
        )
    }

    /// It changes the dab's colour.
    pub fn is_color(self) -> bool {
        matches!(
            self,
            DabSetting::Hue
                | DabSetting::Saturation
                | DabSetting::Value
                | DabSetting::ColorMix
                | DabSetting::Darken
        )
    }
}

/// "This input drives that setting": any sensor to any dab setting, through
/// its own curve, like MyPaint's dynamics. They stack with
/// the brush's fixed dynamics (pressure, tapers, speed, randomness).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InputMapping {
    pub sensor: Sensor,
    pub setting: DabSetting,
    /// -1..1. For size and opacity, positive: a high input keeps it full
    /// and a low one reduces it; negative: a high input reduces it. For the
    /// rest, how far a full input moves it (negative: the other way).
    pub amount: f32,
    /// Input (0..1) to how much of it counts (0..1).
    pub curve: crate::brush_engine::hardness::SoftnessCurve,
    /// Distance (px), time (s) or dabs that count as the full input.
    pub length: f32,
    /// Distance, time or dabs start over from 0 every `length`, rather
    /// than staying full.
    pub periodic: bool,
    /// The input swings the setting both ways, the middle of its range
    /// leaving it alone; for
    /// the settings that add (angle, colour, hardness, scatter).
    pub both_ways: bool,
}

impl Default for InputMapping {
    fn default() -> Self {
        Self {
            sensor: Sensor::Pressure,
            setting: DabSetting::Size,
            amount: 1.0,
            curve: crate::brush_engine::hardness::SoftnessCurve {
                points: vec![
                    crate::brush_engine::hardness::CurvePoint::new(0.0, 0.0),
                    crate::brush_engine::hardness::CurvePoint::new(1.0, 1.0),
                ],
            },
            length: 200.0,
            periodic: false,
            both_ways: false,
        }
    }
}

/// Every sensor's reading (0..1) for one dab.
#[derive(Clone, Copy, Debug, Default)]
pub struct SensorValues {
    pub pressure: f32,
    pub pressure_in: f32,
    pub speed: f32,
    pub tilt: f32,
    pub tilt_direction: f32,
    pub direction: f32,
    /// Raw, in px and s: the mapping's length scales them.
    pub distance: f32,
    pub time: f32,
    pub random_dab: f32,
    pub random_stroke: f32,
    pub rotation: f32,
    pub wheel: f32,
    /// Dabs since the stroke started (raw, like distance).
    pub dabs: f32,
    pub perspective: f32,
    pub x_tilt: f32,
    pub y_tilt: f32,
}

impl InputMapping {
    /// The input after its curve, 0..1.
    pub fn input(&self, s: &SensorValues) -> f32 {
        let len = self.length.max(1e-3);
        let along = |v: f32| {
            if self.periodic {
                v.rem_euclid(len) / len
            } else {
                v / len
            }
        };
        let raw = match self.sensor {
            Sensor::Pressure => s.pressure,
            Sensor::Speed => s.speed,
            Sensor::Tilt => s.tilt,
            Sensor::TiltDirection => s.tilt_direction,
            Sensor::Direction => s.direction,
            Sensor::Distance => along(s.distance),
            Sensor::Time => along(s.time),
            Sensor::Fade => along(s.dabs),
            Sensor::RandomDab => s.random_dab,
            Sensor::RandomStroke => s.random_stroke,
            Sensor::Rotation => s.rotation,
            Sensor::Wheel => s.wheel,
            Sensor::PressureIn => s.pressure_in,
            Sensor::Perspective => s.perspective,
            Sensor::XTilt => s.x_tilt,
            Sensor::YTilt => s.y_tilt,
        };
        self.curve.eval(raw.clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }

    /// Apply this mapping to `v` given the sensors.
    pub fn apply(&self, v: &mut DabVar, s: &SensorValues) {
        let x = self.input(s);
        let a = self.amount.clamp(-1.0, 1.0);
        // Both ways: an added setting moves by -a..a over the input.
        let added = if self.both_ways { 2.0 * x - 1.0 } else { x };
        // Size and opacity scale: full at one end of the input, reduced
        // by the amount at the other.
        let factor = if a >= 0.0 {
            1.0 - a * (1.0 - x)
        } else {
            1.0 + a * x
        };
        match self.setting {
            DabSetting::Size => v.scale *= factor.max(0.0),
            DabSetting::Opacity => v.strength *= factor.max(0.0),
            DabSetting::Angle => v.turn += a * added * std::f32::consts::PI,
            DabSetting::Squash => v.squash *= (1.0 - a.abs() * x).max(0.05),
            DabSetting::Hue => v.hsv[0] += a * added * 180.0,
            DabSetting::Saturation => v.hsv[1] += a * added,
            DabSetting::Value => v.hsv[2] += a * added,
            DabSetting::TextureStrength => v.texture *= factor.max(0.0),
            DabSetting::Hardness => v.hardness += a * added,
            DabSetting::Scatter => v.scatter += a * added,
            DabSetting::ColorMix => v.mix += a * x,
            DabSetting::Sharpness => v.sharpness *= factor.max(0.0),
            DabSetting::Darken => v.darken *= factor.max(0.0),
            DabSetting::Softness => v.softness *= factor.max(0.0),
            DabSetting::Mirror => v.mirror *= factor.max(0.0),
            DabSetting::SketchDensity => v.sketch[0] *= factor.max(0.0),
            DabSetting::SketchWidth => v.sketch[1] *= factor.max(0.0),
            DabSetting::SketchOffset => v.sketch[2] *= factor.max(0.0),
            DabSetting::Flow => v.flow *= factor.max(0.0),
            DabSetting::Spacing => v.spacing *= factor.max(0.05),
            DabSetting::Lightness => v.lightness *= factor.max(0.0),
        }
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
    /// Which of the brush's tips (see `BrushOptions::tip_count`).
    pub tip: u8,
    /// How far along the stroke the dab is, canvas pixels (for a bristle
    /// brush's hairs running dry).
    pub along: f32,
    /// A hatching brush: how many directions it hatches in (1..=3).
    pub hatch: u8,
    /// Extra turn of the tip (radians) and squash factor from the brush's
    /// input mappings, folded into `orient`.
    pub turn: f32,
    pub squash: f32,
    /// Texture strength factor (1 = the brush's).
    pub texture: f32,
    /// Hardness added to the brush's (0..1 is the whole range).
    pub hardness: f32,
    /// Scatter added to the brush's, in brush widths.
    pub scatter: f32,
    /// How much of the secondary colour is mixed in (0..1).
    pub mix: f32,
    /// Hard edges' strength factor (1 = the brush's).
    pub sharpness: f32,
    /// Colour factor toward black (1 = the colour as it is).
    pub darken: f32,
    /// Softness factor (1 = the tip as it is; see [`DabSetting::Softness`]).
    pub softness: f32,
    /// The Mirror input (see [`DabSetting::Mirror`]): 1 with none.
    pub mirror: f32,
    /// A sketch brush's density, line width and line offset factors.
    pub sketch: [f32; 3],
    /// Flow, spacing and lightness strength factors (1 = the brush's).
    pub flow: f32,
    pub spacing: f32,
    pub lightness: f32,
    /// A colour (sRGB 0..1) in place of the brush colour (a random colour
    /// source).
    pub base: Option<[f32; 3]>,
}

impl Default for DabVar {
    fn default() -> Self {
        Self {
            scale: 1.0,
            strength: 1.0,
            orient: IDENTITY,
            hsv: [0.0; 3],
            tip: 0,
            along: 0.0,
            hatch: 1,
            turn: 0.0,
            squash: 1.0,
            texture: 1.0,
            hardness: 0.0,
            scatter: 0.0,
            mix: 0.0,
            sharpness: 1.0,
            darken: 1.0,
            softness: 1.0,
            mirror: 1.0,
            sketch: [1.0; 3],
            flow: 1.0,
            spacing: 1.0,
            lightness: 1.0,
            base: None,
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
    let [r, g, b, _] = color.unmultiplied().map(|v| v as f32 / 255.0);
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

/// A perspective plane, for the Perspective sensor: its four corners in
/// order round it, and its horizon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PerspectiveGrid {
    corners: [Vec2; 4],
    /// A point on the horizon, its direction (unit), and the corner
    /// farthest from it.
    horizon: (Vec2, Vec2),
    farthest: f32,
}

impl PerspectiveGrid {
    /// The plane through `corners`; `None` if it has no vanishing point
    /// (a parallelogram), which reads 1 all over.
    pub fn new(corners: [Vec2; 4]) -> Option<Self> {
        let [a, b, c, d] = corners;
        // Where the sides a–b and d–c meet, and a–d and b–c.
        let meet = |p: Vec2, q: Vec2, r: Vec2, s: Vec2| {
            let (u, v) = (q - p, s - r);
            let denom = u.x * v.y - u.y * v.x;
            (denom.abs() > 1e-6 * u.length() * v.length())
                .then(|| p + u * (((r - p).x * v.y - (r - p).y * v.x) / denom))
        };
        let horizon = match (meet(a, b, d, c), meet(a, d, b, c)) {
            (Some(p), Some(q)) if (q - p).length() > 1e-6 => (p, (q - p).normalized()),
            // One vanishing point: the horizon runs through it along the
            // parallel sides.
            (Some(p), None) => (p, (d - a).normalized()),
            (None, Some(q)) => (q, (b - a).normalized()),
            _ => return None,
        };
        let mut grid = Self {
            corners,
            horizon,
            farthest: 0.0,
        };
        grid.farthest = corners
            .iter()
            .map(|&p| grid.off_horizon(p))
            .fold(0.0, f32::max);
        (grid.farthest > 0.0).then_some(grid)
    }

    fn off_horizon(&self, p: Vec2) -> f32 {
        let (o, dir) = self.horizon;
        let d = p - o;
        (d.x * dir.y - d.y * dir.x).abs()
    }

    /// Whether `p` is on the plane (inside its four corners).
    fn contains(&self, p: Vec2) -> bool {
        let mut inside = false;
        for i in 0..4 {
            let (a, b) = (self.corners[i], self.corners[(i + 1) % 4]);
            if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
                inside = !inside;
            }
        }
        inside
    }

    /// The Perspective sensor at `p`: the first of `grids` it is on, its
    /// distance from that one's horizon over its farthest corner's; 1 off
    /// them all.
    pub fn sensor(grids: &[PerspectiveGrid], p: Vec2) -> f32 {
        grids
            .iter()
            .find(|g| g.contains(p))
            .map_or(1.0, |g| g.off_horizon(p) / g.farthest)
    }
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
    fn perspective_reads_the_distance_from_the_horizon() {
        // A floor seen in two-point perspective: its far side (a–b) is
        // shorter than its near one (d–c), and its sides lean in.
        let corners = [
            Vec2::new(40.0, 40.0),
            Vec2::new(60.0, 40.0),
            Vec2::new(90.0, 90.0),
            Vec2::new(0.0, 80.0),
        ];
        let grid = PerspectiveGrid::new(corners).unwrap();
        let grids = [grid];
        let far = PerspectiveGrid::sensor(&grids, Vec2::new(50.0, 42.0));
        let near = PerspectiveGrid::sensor(&grids, Vec2::new(50.0, 82.0));
        assert!(far < near && near <= 1.0, "{far} {near}");
        // The farthest corner reads 1; off the plane, 1 too.
        assert!((grid.off_horizon(corners[2]) / grid.farthest - 1.0).abs() < 1e-4);
        assert_eq!(PerspectiveGrid::sensor(&grids, Vec2::new(200.0, 5.0)), 1.0);
        // A rectangle has no vanishing point.
        let flat = [
            Vec2::ZERO,
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(0.0, 10.0),
        ];
        assert!(PerspectiveGrid::new(flat).is_none());
    }

    #[test]
    fn fade_counts_dabs_and_lengths_can_repeat() {
        let mut m = InputMapping {
            sensor: Sensor::Fade,
            length: 10.0,
            ..Default::default()
        };
        let at = |m: &InputMapping, dabs: f32| {
            m.input(&SensorValues {
                dabs,
                ..Default::default()
            })
        };
        assert_eq!(at(&m, 5.0), 0.5);
        assert_eq!(at(&m, 25.0), 1.0);
        m.periodic = true;
        assert_eq!(at(&m, 25.0), 0.5);
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
