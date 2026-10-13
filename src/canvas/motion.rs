//! Keyed motion: a layer's position, scale, rotation and opacity set at
//! some frames (its keys) and filled in between along each key's easing
//! curve, as in Alight Motion or After Effects. A layer can follow another
//! (its parent) and follows the folder it's in, so an animated layer's
//! drawings move with it.
//!
//! Its look can be keyed too: blur, brightness, contrast, saturation, hue
//! and a tint, which a folder's layers take from it.
//!
//! Motion doesn't change a layer's pixels: they stay where they were
//! painted, and the canvas shows them moved (see `storage::posed`). Painting
//! on a moved layer paints where the pointer is on its pixels.

use super::rig::eval::sample;
use super::rig::{Affine, Curve, Key};
use super::storage::{Anim, Canvas, LayerId, LayerKind};
use serde::{Deserialize, Serialize};

/// What a key sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Prop {
    /// How far the layer has moved (pixels).
    Position,
    /// Its size (1 is as painted), across and down.
    Scale,
    /// Its turn about its anchor (degrees, clockwise).
    Rotation,
    /// How opaque it is (0 to 1).
    Opacity,
    /// The point (on its own pixels) it turns and scales about.
    Anchor,
    /// How blurred (pixels).
    Blur,
    /// Lighter or darker (-1 to 1).
    Brightness,
    /// More or less contrast (-1 to 1).
    Contrast,
    /// How colourful (0 grey, 1 as painted, up to 2).
    Saturation,
    /// Colours turned round the wheel (degrees).
    Hue,
    /// How far towards the tint colour (0 to 1).
    Tint,
}

impl Prop {
    pub const ALL: [Prop; 11] = [
        Prop::Position,
        Prop::Scale,
        Prop::Rotation,
        Prop::Opacity,
        Prop::Anchor,
        Prop::Blur,
        Prop::Brightness,
        Prop::Contrast,
        Prop::Saturation,
        Prop::Hue,
        Prop::Tint,
    ];
    /// Where it is and how opaque.
    pub const TRANSFORM: [Prop; 5] = [
        Prop::Position,
        Prop::Scale,
        Prop::Rotation,
        Prop::Opacity,
        Prop::Anchor,
    ];
    /// How it looks.
    pub const EFFECTS: [Prop; 6] = [
        Prop::Blur,
        Prop::Brightness,
        Prop::Contrast,
        Prop::Saturation,
        Prop::Hue,
        Prop::Tint,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Prop::Position => "Position",
            Prop::Scale => "Scale",
            Prop::Rotation => "Rotation",
            Prop::Opacity => "Opacity",
            Prop::Anchor => "Pivot",
            Prop::Blur => "Blur",
            Prop::Brightness => "Brightness",
            Prop::Contrast => "Contrast",
            Prop::Saturation => "Saturation",
            Prop::Hue => "Hue",
            Prop::Tint => "Tint",
        }
    }

    /// Its value with no keys (one-number properties use the first). The
    /// pivot's is the motion's own (see [`Motion::value`]).
    pub fn rest(self) -> [f32; 2] {
        match self {
            Prop::Scale => [1.0, 1.0],
            Prop::Opacity | Prop::Saturation => [1.0, 0.0],
            _ => [0.0, 0.0],
        }
    }

    /// How many of a value's two numbers it uses.
    pub fn dims(self) -> usize {
        match self {
            Prop::Position | Prop::Scale | Prop::Anchor => 2,
            _ => 1,
        }
    }

    pub fn is_effect(self) -> bool {
        Prop::EFFECTS.contains(&self)
    }
}

/// How a layer looks: its keyed effects at a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub blur: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub hue: f32,
    pub tint: f32,
    pub tint_color: [u8; 3],
}

impl Default for Look {
    fn default() -> Self {
        Look {
            blur: 0.0,
            brightness: 0.0,
            contrast: 0.0,
            saturation: 1.0,
            hue: 0.0,
            tint: 0.0,
            tint_color: DEFAULT_TINT,
        }
    }
}

/// A new tint's colour.
const DEFAULT_TINT: [u8; 3] = [255, 150, 40];

impl Look {
    /// Leaves pixels as they are.
    pub fn is_plain(&self) -> bool {
        self.blur < 0.25
            && self.brightness.abs() < 1e-3
            && self.contrast.abs() < 1e-3
            && (self.saturation - 1.0).abs() < 1e-3
            && self.hue.abs() < 1e-2
            && self.tint < 1e-3
    }

    /// This look over `inner` (a folder's over its layer's).
    pub fn over(&self, inner: &Look) -> Look {
        Look {
            blur: (self.blur * self.blur + inner.blur * inner.blur).sqrt(),
            brightness: self.brightness + inner.brightness,
            contrast: (1.0 + self.contrast) * (1.0 + inner.contrast) - 1.0,
            saturation: self.saturation * inner.saturation,
            hue: self.hue + inner.hue,
            tint: if inner.tint >= self.tint {
                inner.tint
            } else {
                self.tint
            },
            tint_color: if inner.tint >= self.tint {
                inner.tint_color
            } else {
                self.tint_color
            },
        }
    }

    /// Pixel `p` (premultiplied, 0 to 255) as this look shows it.
    pub fn apply(&self, [r, g, b, a]: [f32; 4]) -> [f32; 4] {
        if a <= 0.0 {
            return [0.0; 4];
        }
        let mut c = [r, g, b];
        if self.hue.abs() >= 1e-2 {
            let (s, k) = self.hue.to_radians().sin_cos();
            let m = [
                [
                    0.213 + 0.787 * k - 0.213 * s,
                    0.715 - 0.715 * k - 0.715 * s,
                    0.072 - 0.072 * k + 0.928 * s,
                ],
                [
                    0.213 - 0.213 * k + 0.143 * s,
                    0.715 + 0.285 * k + 0.140 * s,
                    0.072 - 0.072 * k - 0.283 * s,
                ],
                [
                    0.213 - 0.213 * k - 0.787 * s,
                    0.715 - 0.715 * k + 0.715 * s,
                    0.072 + 0.928 * k + 0.072 * s,
                ],
            ];
            c = [0, 1, 2].map(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2]);
        }
        if (self.saturation - 1.0).abs() >= 1e-3 {
            let grey = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            c = c.map(|v| grey + (v - grey) * self.saturation);
        }
        // (In premultiplied terms: mid grey and white scale with alpha.)
        if self.contrast.abs() >= 1e-3 {
            let mid = 128.0 * a / 255.0;
            c = c.map(|v| (v - mid) * (1.0 + self.contrast) + mid);
        }
        if self.brightness.abs() >= 1e-3 {
            c = c.map(|v| v + self.brightness * a);
        }
        if self.tint >= 1e-3 {
            let t = self.tint.clamp(0.0, 1.0);
            for (v, tc) in c.iter_mut().zip(self.tint_color) {
                *v += (tc as f32 * a / 255.0 - *v) * t;
            }
        }
        let c = c.map(|v| v.clamp(0.0, a));
        [c[0], c[1], c[2], a]
    }
}

/// Named easing curves (a key's curve takes the value to the next key).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ease {
    Linear,
    In,
    Out,
    InOut,
    /// Past the next key's value and back.
    Overshoot,
    /// Holds, then jumps to the next key.
    Hold,
}

impl Ease {
    pub const ALL: [Ease; 6] = [
        Ease::Linear,
        Ease::In,
        Ease::Out,
        Ease::InOut,
        Ease::Overshoot,
        Ease::Hold,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Ease::Linear => "Linear",
            Ease::In => "Ease in",
            Ease::Out => "Ease out",
            Ease::InOut => "Ease in & out",
            Ease::Overshoot => "Overshoot",
            Ease::Hold => "Hold",
        }
    }

    pub fn curve(self) -> Curve {
        match self {
            Ease::Linear => Curve::Linear,
            Ease::In => Curve::Bezier([0.42, 0.0, 1.0, 1.0]),
            Ease::Out => Curve::Bezier([0.0, 0.0, 0.58, 1.0]),
            Ease::InOut => Curve::Bezier([0.42, 0.0, 0.58, 1.0]),
            Ease::Overshoot => Curve::Bezier([0.34, 1.56, 0.64, 1.0]),
            Ease::Hold => Curve::Stepped,
        }
    }

    /// The preset `curve` is, if any.
    pub fn of(curve: Curve) -> Option<Ease> {
        Ease::ALL.into_iter().find(|e| e.curve() == curve)
    }
}

/// A layer's keys, and what it moves about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Motion {
    /// The point (on the layer's pixels) it turns and scales about.
    pub anchor: [f32; 2],
    /// Keys at frames (a key's `time` is its frame), for each property.
    pub position: Vec<Key<[f32; 2]>>,
    pub scale: Vec<Key<[f32; 2]>>,
    pub rotation: Vec<Key<[f32; 2]>>,
    pub opacity: Vec<Key<[f32; 2]>>,
    /// Keys moving the pivot (`anchor` without any).
    pub anchor_keys: Vec<Key<[f32; 2]>>,
    pub blur: Vec<Key<[f32; 2]>>,
    pub brightness: Vec<Key<[f32; 2]>>,
    pub contrast: Vec<Key<[f32; 2]>>,
    pub saturation: Vec<Key<[f32; 2]>>,
    pub hue: Vec<Key<[f32; 2]>>,
    pub tint: Vec<Key<[f32; 2]>>,
    /// The colour its tint goes towards.
    pub tint_color: [u8; 3],
    /// The layer it follows (its id), moving as that one does.
    pub parent: Option<u64>,
}

impl Default for Motion {
    fn default() -> Self {
        Motion {
            anchor: [0.0, 0.0],
            position: Vec::new(),
            scale: Vec::new(),
            rotation: Vec::new(),
            opacity: Vec::new(),
            anchor_keys: Vec::new(),
            blur: Vec::new(),
            brightness: Vec::new(),
            contrast: Vec::new(),
            saturation: Vec::new(),
            hue: Vec::new(),
            tint: Vec::new(),
            tint_color: DEFAULT_TINT,
            parent: None,
        }
    }
}

impl Motion {
    pub fn new(anchor: [f32; 2]) -> Self {
        Self {
            anchor,
            ..Default::default()
        }
    }

    pub fn keys(&self, p: Prop) -> &Vec<Key<[f32; 2]>> {
        match p {
            Prop::Position => &self.position,
            Prop::Scale => &self.scale,
            Prop::Rotation => &self.rotation,
            Prop::Opacity => &self.opacity,
            Prop::Anchor => &self.anchor_keys,
            Prop::Blur => &self.blur,
            Prop::Brightness => &self.brightness,
            Prop::Contrast => &self.contrast,
            Prop::Saturation => &self.saturation,
            Prop::Hue => &self.hue,
            Prop::Tint => &self.tint,
        }
    }

    pub fn keys_mut(&mut self, p: Prop) -> &mut Vec<Key<[f32; 2]>> {
        match p {
            Prop::Position => &mut self.position,
            Prop::Scale => &mut self.scale,
            Prop::Rotation => &mut self.rotation,
            Prop::Opacity => &mut self.opacity,
            Prop::Anchor => &mut self.anchor_keys,
            Prop::Blur => &mut self.blur,
            Prop::Brightness => &mut self.brightness,
            Prop::Contrast => &mut self.contrast,
            Prop::Saturation => &mut self.saturation,
            Prop::Hue => &mut self.hue,
            Prop::Tint => &mut self.tint,
        }
    }

    /// No keys and no parent: it doesn't move.
    pub fn is_still(&self) -> bool {
        Prop::ALL.iter().all(|&p| self.keys(p).is_empty()) && self.parent.is_none()
    }

    /// `p`'s value at frame `t` (between frames too).
    pub fn value(&self, p: Prop, t: f32) -> [f32; 2] {
        let rest = if p == Prop::Anchor {
            self.anchor
        } else {
            p.rest()
        };
        sample(self.keys(p), t).unwrap_or(rest)
    }

    /// Its own look at frame `t`.
    pub fn look(&self, t: f32) -> Look {
        let v = |p| self.value(p, t)[0];
        Look {
            blur: v(Prop::Blur).max(0.0),
            brightness: v(Prop::Brightness),
            contrast: v(Prop::Contrast).max(-1.0),
            saturation: v(Prop::Saturation).max(0.0),
            hue: v(Prop::Hue),
            tint: v(Prop::Tint).clamp(0.0, 1.0),
            tint_color: self.tint_color,
        }
    }

    /// The properties with keys, of `props`.
    pub fn keyed(&self, props: &[Prop]) -> Vec<Prop> {
        props
            .iter()
            .copied()
            .filter(|&p| !self.keys(p).is_empty())
            .collect()
    }

    /// The frames `p` has keys on.
    pub fn key_frames(&self, p: Prop) -> Vec<u32> {
        self.keys(p).iter().map(|k| k.time.round() as u32).collect()
    }

    /// Every frame with a key, in order.
    pub fn all_key_frames(&self) -> Vec<u32> {
        let mut all: Vec<u32> = Prop::ALL.iter().flat_map(|&p| self.key_frames(p)).collect();
        all.sort_unstable();
        all.dedup();
        all
    }

    fn find(&self, p: Prop, frame: u32) -> Option<usize> {
        self.keys(p)
            .iter()
            .position(|k| k.time.round() as u32 == frame)
    }

    pub fn key_at(&self, p: Prop, frame: u32) -> Option<&Key<[f32; 2]>> {
        self.find(p, frame).map(|i| &self.keys(p)[i])
    }

    /// Key `p` at `frame` to `value` (a new key takes the easing of the
    /// key before it).
    pub fn set(&mut self, p: Prop, frame: u32, value: [f32; 2]) {
        if let Some(i) = self.find(p, frame) {
            self.keys_mut(p)[i].value = value;
            return;
        }
        let keys = self.keys_mut(p);
        let at = keys.partition_point(|k| k.time < frame as f32);
        let curve = at
            .checked_sub(1)
            .map_or(Curve::Linear, |before| keys[before].curve);
        keys.insert(
            at,
            Key {
                time: frame as f32,
                value,
                curve,
            },
        );
    }

    pub fn remove(&mut self, p: Prop, frame: u32) -> bool {
        match self.find(p, frame) {
            Some(i) => {
                self.keys_mut(p).remove(i);
                true
            }
            None => false,
        }
    }

    /// Move `p`'s key from frame `from` to `to` (unless one is there).
    pub fn move_key(&mut self, p: Prop, from: u32, to: u32) -> bool {
        if from == to || self.find(p, to).is_some() {
            return false;
        }
        let Some(i) = self.find(p, from) else {
            return false;
        };
        let mut key = self.keys_mut(p).remove(i);
        key.time = to as f32;
        let keys = self.keys_mut(p);
        let at = keys.partition_point(|k| k.time < to as f32);
        keys.insert(at, key);
        true
    }

    pub fn set_curve(&mut self, p: Prop, frame: u32, curve: Curve) -> bool {
        match self.find(p, frame) {
            Some(i) => {
                self.keys_mut(p)[i].curve = curve;
                true
            }
            None => false,
        }
    }

    /// Its own transform (pixels to where they show) and opacity at frame
    /// `t`.
    pub fn local(&self, t: f32) -> (Affine, f32) {
        let [dx, dy] = self.value(Prop::Position, t);
        let [sx, sy] = self.value(Prop::Scale, t);
        let turn = self.value(Prop::Rotation, t)[0].to_radians();
        let opacity = self.value(Prop::Opacity, t)[0].clamp(0.0, 1.0);
        let (sin, cos) = turn.sin_cos();
        let m = [cos * sx, -sin * sy, sin * sx, cos * sy];
        let [ax, ay] = self.value(Prop::Anchor, t);
        let turned = [m[0] * ax + m[1] * ay, m[2] * ax + m[3] * ay];
        let affine = Affine {
            m,
            t: [ax + dx - turned[0], ay + dy - turned[1]],
        };
        (affine, opacity)
    }
}

/// Whether a transform and opacity leave pixels as they are.
pub fn is_identity(affine: &Affine, opacity: f32) -> bool {
    let i = Affine::IDENTITY;
    (0..4).all(|k| (affine.m[k] - i.m[k]).abs() < 1e-5)
        && affine.t.iter().all(|t| t.abs() < 1e-3)
        && opacity > 0.9999
}

impl Canvas {
    /// Where layer `i`'s pixels show at the current frame, and how opaque:
    /// its own keys after those of the layer it follows, or else of its
    /// folder. A mask moves with its layer.
    pub fn world_motion(&self, i: usize) -> (Affine, f32) {
        self.world_motion_at(i, self.time as f32, 0)
    }

    /// [`Self::world_motion`] at frame `t`.
    pub fn world_motion_at(&self, i: usize, t: f32, depth: u32) -> (Affine, f32) {
        let identity = (Affine::IDENTITY, 1.0);
        let Some(layer) = self.layers.get(i) else {
            return identity;
        };
        // (A loop of parents goes no further.)
        if depth > 32 {
            return identity;
        }
        if let LayerKind::Mask { owner } = layer.kind {
            return match self.layer_index_of(owner) {
                Some(o) => self.world_motion_at(o, t, depth + 1),
                None => identity,
            };
        }
        let followed = (layer.motion.as_ref())
            .and_then(|m| m.parent)
            .and_then(|id| self.layer_index_of(LayerId(id)))
            .filter(|&p| p != i);
        let (base, base_opacity) =
            match (followed, layer.parent.and_then(|p| self.layer_index_of(p))) {
                // A followed layer's motion, not its opacity.
                (Some(p), Some(folder)) => {
                    let (affine, _) = self.world_motion_at(p, t, depth + 1);
                    (affine, self.world_motion_at(folder, t, depth + 1).1)
                }
                (Some(p), None) => (self.world_motion_at(p, t, depth + 1).0, 1.0),
                (None, Some(folder)) => self.world_motion_at(folder, t, depth + 1),
                (None, None) => identity,
            };
        match &layer.motion {
            Some(m) => {
                let (local, opacity) = m.local(t);
                (base.then(&local), base_opacity * opacity)
            }
            None => (base, base_opacity),
        }
    }

    /// How layer `i` looks at the current frame: its keyed effects over
    /// those of the folders it's in.
    pub fn world_look(&self, i: usize) -> Look {
        self.world_look_at(i, self.time as f32, 0)
    }

    fn world_look_at(&self, i: usize, t: f32, depth: u32) -> Look {
        let Some(layer) = self.layers.get(i) else {
            return Look::default();
        };
        if depth > 32 || matches!(layer.kind, LayerKind::Mask { .. }) {
            return Look::default();
        }
        let own = layer
            .motion
            .as_ref()
            .map_or_else(Look::default, |m| m.look(t));
        match layer.parent.and_then(|p| self.layer_index_of(p)) {
            Some(folder) => self.world_look_at(folder, t, depth + 1).over(&own),
            None => own,
        }
    }

    /// Whether layer `i` moves (or fades) at the current frame.
    pub fn is_moved(&self, i: usize) -> bool {
        let (affine, opacity) = self.world_motion(i);
        !is_identity(&affine, opacity)
    }

    /// Whether any layer has keys or follows another.
    pub fn has_motion(&self) -> bool {
        self.layers
            .iter()
            .any(|l| l.motion.as_ref().is_some_and(|m| !m.is_still()))
    }

    /// The box (x0, y0, x1, y1 on its own pixels) around what layer `i`
    /// shows now: a folder's or animated layer's, what its layers show.
    /// `None` if it shows nothing.
    pub fn content_rect(&self, i: usize) -> Option<[f32; 4]> {
        let layer = self.layers.get(i)?;
        let union = |a: Option<[f32; 4]>, b: Option<[f32; 4]>| match (a, b) {
            (Some(a), Some(b)) => Some([
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]),
            (a, b) => a.or(b),
        };
        if layer.kind == LayerKind::Group {
            let id = layer.id;
            return (0..self.layers.len())
                .filter(|&c| self.layers[c].parent == Some(id))
                .filter(|&c| self.shown_in_time(c))
                .filter(|&c| !matches!(self.layers[c].kind, LayerKind::Mask { .. }))
                .fold(None, |acc, c| union(acc, self.content_rect(c)));
        }
        if i == 0 {
            return Some([0.0, 0.0, self.width() as f32, self.height() as f32]);
        }
        let ts = self.tile_size();
        let mut rect: Option<[f32; 4]> = None;
        for (tx, ty) in self.layer_tile_keys(i) {
            let Some(data) = self.get_layer_tile_data(i, tx, ty) else {
                continue;
            };
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
            for (k, p) in data.iter().enumerate() {
                if p.a() > 0 {
                    let (x, y) = (k % ts, k / ts);
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
            if x0 <= x1 && x0 != usize::MAX {
                let (ox, oy) = (
                    (tx as i64 * ts as i64) as f32,
                    (ty as i64 * ts as i64) as f32,
                );
                let tile = [
                    ox + x0 as f32,
                    oy + y0 as f32,
                    ox + x1 as f32,
                    oy + y1 as f32,
                ];
                rect = union(rect, Some(tile));
            }
        }
        rect
    }

    /// Whether drawing `i` (of an animated layer) has nothing on it: an
    /// empty exposure, the layer showing nothing for those frames.
    pub fn is_blank_drawing(&self, i: usize) -> bool {
        let Some(layer) = self.layers.get(i) else {
            return false;
        };
        matches!(layer.anim, Some(Anim::Frame(_)))
            && self.layer_tile_keys(i).into_iter().all(|(tx, ty)| {
                self.get_layer_tile_data(i, tx, ty)
                    .is_none_or(|d| d.iter().all(|p| p.a() == 0))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    #[test]
    fn keys_are_set_moved_and_eased_between() {
        let mut m = Motion::new([10.0, 10.0]);
        m.set(Prop::Position, 0, [0.0, 0.0]);
        m.set(Prop::Position, 10, [100.0, 0.0]);
        assert_eq!(m.value(Prop::Position, 5.0), [50.0, 0.0]);
        assert_eq!(m.value(Prop::Position, 20.0), [100.0, 0.0], "held after");
        m.set_curve(Prop::Position, 0, Ease::Hold.curve());
        assert_eq!(m.value(Prop::Position, 9.0), [0.0, 0.0]);
        m.set_curve(Prop::Position, 0, Ease::InOut.curve());
        assert!(m.value(Prop::Position, 2.0)[0] < 20.0, "slow to start");
        assert!(m.move_key(Prop::Position, 10, 20));
        assert_eq!(m.key_frames(Prop::Position), [0, 20]);
        m.set(Prop::Rotation, 5, [90.0, 0.0]);
        assert_eq!(m.all_key_frames(), [0, 5, 20]);
        assert!(m.remove(Prop::Rotation, 5));
        assert_eq!(m.all_key_frames(), [0, 20]);
    }

    #[test]
    fn a_turn_goes_about_the_anchor() {
        let mut m = Motion::new([10.0, 10.0]);
        m.set(Prop::Rotation, 0, [90.0, 0.0]);
        let (a, _) = m.local(0.0);
        let anchor = a.apply([10.0, 10.0]);
        assert!((anchor[0] - 10.0).abs() < 1e-4 && (anchor[1] - 10.0).abs() < 1e-4);
        let right = a.apply([20.0, 10.0]);
        assert!(
            (right[0] - 10.0).abs() < 1e-4 && (right[1] - 20.0).abs() < 1e-4,
            "{right:?}"
        );
    }

    #[test]
    fn a_layer_follows_its_parent_and_its_folder() {
        let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let a = canvas.layers[1].id;
        let b = canvas.insert_new_layer(2, "B".into(), LayerKind::Paint, None);
        let mut leader = Motion::new([0.0, 0.0]);
        leader.set(Prop::Position, 0, [5.0, 0.0]);
        canvas.layers[1].motion = Some(Box::new(leader));
        let bi = canvas.layer_index_of(b).unwrap();
        let mut follower = Motion::new([0.0, 0.0]);
        follower.set(Prop::Position, 0, [0.0, 3.0]);
        follower.parent = Some(a.0);
        canvas.layers[bi].motion = Some(Box::new(follower));
        let (affine, _) = canvas.world_motion(bi);
        assert_eq!(affine.apply([0.0, 0.0]), [5.0, 3.0]);
        assert!(canvas.is_moved(bi) && canvas.has_motion());
    }
}
