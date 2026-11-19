//! Hand tracking with MediaPipe's two hand models (Apache-2.0, as ONNX
//! from OpenCV's model zoo): a palm detector finds a hand in the whole
//! picture, then a landmark model reads 21 points of it from a square crop
//! turned so the fingers point up. While the landmarks keep coming, the next
//! frame's crop is taken from them and the detector is skipped.
//!
//! Landmarks are numbered as MediaPipe numbers them: 0 the wrist, then four
//! a finger from its base to its tip, thumb first (1-4: CMC, MCP, IP, tip;
//! 5-8 the index: MCP, PIP, DIP, tip; 9-12 middle; 13-16 ring; 17-20
//! little).

use std::path::Path;
use std::sync::Arc;

use glam::{Vec2, Vec3};

use super::crop::{Layout, Roi, crop, sigmoid};
use image::RgbImage;
use tract_onnx::prelude::*;

/// How many landmarks a hand has.
pub const LANDMARKS: usize = 21;

/// The palm detector's input side, in pixels.
const PALM_SIZE: usize = 192;
/// The landmark model's.
const HAND_SIZE: usize = 224;
/// A palm counts from this score.
const PALM_SCORE: f32 = 0.5;
/// Palms overlapping more than this are one.
const PALM_OVERLAP: f32 = 0.3;
/// The landmarks count (and the hand stays tracked) from this confidence.
pub const HAND_CONFIDENCE: f32 = 0.5;

pub(super) type Plan = Arc<TypedRunnableModel>;

/// Which hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handedness {
    Left,
    Right,
}

/// A palm the detector found, in the picture's pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palm {
    pub score: f32,
    /// Its box's corners (top left, bottom right).
    pub min: Vec2,
    pub max: Vec2,
    /// Seven points: the wrist, the index, middle, ring and little
    /// fingers' bases, and two of the thumb's.
    pub points: [Vec2; 7],
}

impl Roi {
    /// The crop around a palm the detector found: turned so the wrist is
    /// below the middle finger's base, moved towards the fingers and grown
    /// to take the whole hand in (MediaPipe's own numbers).
    pub fn from_palm(palm: &Palm) -> Self {
        let up = (palm.points[2] - palm.points[0]).normalize_or(Vec2::NEG_Y);
        let size = palm.max - palm.min;
        let center = (palm.min + palm.max) * 0.5 + up * 0.5 * size.y;
        Self {
            center,
            size: size.max_element() * 2.6,
            up,
        }
    }

    /// The crop for the next frame, around the landmarks of this one.
    pub fn from_landmarks(points: &[Vec3; LANDMARKS]) -> Self {
        const PARTIAL: [usize; 12] = [0, 1, 2, 3, 5, 6, 9, 10, 13, 14, 17, 18];
        let up = (points[9].truncate() - points[0].truncate()).normalize_or(Vec2::NEG_Y);
        let roi = Roi {
            center: Vec2::ZERO,
            size: 1.0,
            up,
        };
        let (right, down) = roi.axes();
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for &i in &PARTIAL {
            let p = points[i].truncate();
            let local = Vec2::new(p.dot(right), p.dot(down));
            lo = lo.min(local);
            hi = hi.max(local);
        }
        let mid = (lo + hi) * 0.5;
        let size = hi - lo;
        let center = right * mid.x + down * mid.y + up * 0.1 * size.y;
        Self {
            center,
            size: size.max_element() * 2.0,
            up,
        }
    }
}

/// One hand read from a picture.
#[derive(Clone, Debug, PartialEq)]
pub struct HandLandmarks {
    /// In the picture: x and y in pixels, z the depth from the wrist (in
    /// the same scale, smaller nearer the camera).
    pub image: [Vec3; LANDMARKS],
    /// In metres round the hand's middle, the picture's axes (x right, y
    /// down, z away from the camera).
    pub world: [Vec3; LANDMARKS],
    /// Which hand it is, for a picture as the camera saw it (not mirrored).
    pub handedness: Handedness,
    /// How sure the model is that it's that hand (0.5..1).
    pub handedness_score: f32,
    /// How sure the model is that it's a hand at all.
    pub confidence: f32,
    /// The crop it was read from.
    pub roi: Roi,
}

/// The two models, ready to run.
pub struct HandModels {
    palm: Plan,
    landmarks: Plan,
    anchors: Vec<Vec2>,
}

/// The models' files in their folder.
pub const PALM_FILE: &str = "palm_detection.onnx";
pub const LANDMARK_FILE: &str = "hand_landmark.onnx";

/// A model loaded from `path` for an input of `shape`, ready to run.
pub(super) fn plan(path: &Path, shape: [usize; 4]) -> TractResult<Plan> {
    tract_onnx::onnx()
        .model_for_path(path)?
        .with_input_fact(0, f32::fact(shape).into())?
        .into_optimized()?
        .into_runnable()
}

impl HandModels {
    /// Load the models from `dir`.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let palm = plan(&dir.join(PALM_FILE), [1, PALM_SIZE, PALM_SIZE, 3])
            .map_err(|e| format!("Couldn't load the palm detector: {e}"))?;
        let landmarks = plan(&dir.join(LANDMARK_FILE), [1, HAND_SIZE, HAND_SIZE, 3])
            .map_err(|e| format!("Couldn't load the hand landmark model: {e}"))?;
        Ok(Self {
            palm,
            landmarks,
            anchors: palm_anchors(),
        })
    }

    /// The palms in `image`, best first.
    pub fn detect(&self, image: &RgbImage) -> Result<Vec<Palm>, String> {
        let (w, h) = (image.width() as f32, image.height() as f32);
        // The whole picture, letterboxed into the detector's square.
        let square = Roi::whole(w, h);
        let input = crop(image, &square, PALM_SIZE, Layout::Nhwc);
        let out = self
            .palm
            .run(tvec!(input.into_tvalue()))
            .map_err(|e| e.to_string())?;
        let mut boxes = None;
        let mut scores = None;
        for t in &out {
            let shape = t.shape();
            let data: Vec<f32> = (t.to_plain_array_view::<f32>())
                .map_err(|e| e.to_string())?
                .iter()
                .copied()
                .collect();
            match shape.last() {
                Some(18) => boxes = Some(data),
                Some(1) => scores = Some(data),
                _ => {}
            }
        }
        let (Some(boxes), Some(scores)) = (boxes, scores) else {
            return Err("The palm detector gave unexpected outputs".into());
        };
        if scores.len() != self.anchors.len() || boxes.len() != self.anchors.len() * 18 {
            return Err("The palm detector's outputs don't match its anchors".into());
        }
        let n = PALM_SIZE as f32;
        let mut palms = Vec::new();
        for (i, (&raw, anchor)) in scores.iter().zip(&self.anchors).enumerate() {
            let score = sigmoid(raw);
            if score < PALM_SCORE {
                continue;
            }
            let r = &boxes[i * 18..(i + 1) * 18];
            let at = |dx: f32, dy: f32| {
                let p = *anchor + Vec2::new(dx, dy) / n;
                square.to_image(p.x, p.y)
            };
            let center = at(r[0], r[1]);
            let half = Vec2::new(r[2], r[3]) / n * square.size * 0.5;
            palms.push(Palm {
                score,
                min: center - half,
                max: center + half,
                points: std::array::from_fn(|k| at(r[4 + 2 * k], r[5 + 2 * k])),
            });
        }
        palms.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut kept: Vec<Palm> = Vec::new();
        for palm in palms {
            if kept.iter().all(|k| overlap(k, &palm) <= PALM_OVERLAP) {
                kept.push(palm);
            }
        }
        Ok(kept)
    }

    /// Read a hand's landmarks from `roi` of `image`.
    pub fn landmarks(&self, image: &RgbImage, roi: &Roi) -> Result<HandLandmarks, String> {
        let input = crop(image, roi, HAND_SIZE, Layout::Nhwc);
        let out = self
            .landmarks
            .run(tvec!(input.into_tvalue()))
            .map_err(|e| e.to_string())?;
        // In the model's order: the landmarks in the crop, how sure it is
        // there's a hand, which hand, the landmarks in metres.
        let mut data = Vec::with_capacity(4);
        for t in &out {
            let values: Vec<f32> = (t.to_plain_array_view::<f32>())
                .map_err(|e| e.to_string())?
                .iter()
                .copied()
                .collect();
            data.push(values);
        }
        let [points, confidence, handedness, world] = &data[..] else {
            return Err("The hand landmark model gave unexpected outputs".into());
        };
        if points.len() != 63 || world.len() != 63 || confidence.len() != 1 || handedness.len() != 1
        {
            return Err("The hand landmark model gave unexpected outputs".into());
        }
        let (confidence, handedness) = (confidence[0], handedness[0]);
        let (right, down) = roi.axes();
        let n = HAND_SIZE as f32;
        let image_points = std::array::from_fn(|i| {
            let p = roi.to_image(points[i * 3] / n, points[i * 3 + 1] / n);
            p.extend(points[i * 3 + 2] / n * roi.size)
        });
        let world_points = std::array::from_fn(|i| {
            let xy = right * world[i * 3] + down * world[i * 3 + 1];
            xy.extend(world[i * 3 + 2])
        });
        // Towards 1 a right hand, towards 0 a left one, as the picture
        // shows it (MediaPipe's own pipeline names the other hand: it
        // mirrors the picture first, as a selfie camera does).
        let (handedness, score) = if handedness >= 0.5 {
            (Handedness::Right, handedness)
        } else {
            (Handedness::Left, 1.0 - handedness)
        };
        Ok(HandLandmarks {
            image: image_points,
            world: world_points,
            handedness,
            handedness_score: score,
            confidence,
            roi: *roi,
        })
    }
}

/// Follows one hand from frame to frame: the detector runs only when
/// there's no hand to follow.
#[derive(Default)]
pub struct HandTracker {
    roi: Option<Roi>,
}

impl HandTracker {
    /// The hand in `image`, if any (sure enough).
    pub fn track(
        &mut self,
        models: &HandModels,
        image: &RgbImage,
    ) -> Result<Option<HandLandmarks>, String> {
        if let Some(roi) = self.roi {
            let hand = models.landmarks(image, &roi)?;
            if hand.confidence >= HAND_CONFIDENCE {
                self.roi = Some(Roi::from_landmarks(&hand.image));
                return Ok(Some(hand));
            }
            self.roi = None;
        }
        let Some(palm) = models.detect(image)?.into_iter().next() else {
            return Ok(None);
        };
        let hand = models.landmarks(image, &Roi::from_palm(&palm))?;
        if hand.confidence < HAND_CONFIDENCE {
            return Ok(None);
        }
        self.roi = Some(Roi::from_landmarks(&hand.image));
        Ok(Some(hand))
    }

    /// Whether a hand is being followed (the detector is skipped).
    #[cfg(test)]
    pub fn is_tracking(&self) -> bool {
        self.roi.is_some()
    }

    /// Forget the hand (a new camera, a jump).
    pub fn reset(&mut self) {
        self.roi = None;
    }
}

/// How much two palms' boxes overlap (intersection over union).
fn overlap(a: &Palm, b: &Palm) -> f32 {
    let lo = a.min.max(b.min);
    let hi = a.max.min(b.max);
    let inter = (hi - lo).max(Vec2::ZERO);
    let inter = inter.x * inter.y;
    let area = |p: &Palm| {
        let s = (p.max - p.min).max(Vec2::ZERO);
        s.x * s.y
    };
    inter / (area(a) + area(b) - inter).max(1e-6)
}

/// The palm detector's anchors (SSD, MediaPipe's options for it): two a
/// cell on a 24×24 grid (stride 8), six a cell on a 12×12 one (three
/// layers of stride 16), cell centres in 0..1.
fn palm_anchors() -> Vec<Vec2> {
    let mut out = Vec::with_capacity(2016);
    for (grid, per_cell) in [(PALM_SIZE / 8, 2), (PALM_SIZE / 16, 6)] {
        for y in 0..grid {
            for x in 0..grid {
                let c = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / grid as f32;
                out.extend(std::iter::repeat_n(c, per_cell));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn models() -> Option<HandModels> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/pose-models/v1");
        if !super::super::is_installed(&dir) {
            eprintln!("the pose models aren't pulled from Git LFS; skipping");
            return None;
        }
        Some(HandModels::load(&dir).unwrap())
    }

    fn photo() -> RgbImage {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/hand-photo.jpg");
        image::open(path).unwrap().to_rgb8()
    }

    #[test]
    fn anchors_match_the_detector() {
        let a = palm_anchors();
        assert_eq!(a.len(), 2016);
        assert!((a[0] - Vec2::splat(0.5 / 24.0)).length() < 1e-6);
        assert!((a[2015] - Vec2::splat(11.5 / 12.0)).length() < 1e-6);
    }

    #[test]
    fn the_models_find_the_left_hand_in_the_photo() {
        let Some(models) = models() else {
            return;
        };
        let image = photo();
        let t = std::time::Instant::now();
        let palms = models.detect(&image).unwrap();
        let detect = t.elapsed();
        assert!(!palms.is_empty(), "no palm found");
        let palm = palms[0];
        // The back of the hand is on the left half, the fingers to the
        // right.
        let mid = (palm.min + palm.max) * 0.5;
        assert!(
            mid.x > 40.0 && mid.x < 220.0 && mid.y > 40.0 && mid.y < 200.0,
            "{palm:?}"
        );
        let roi = Roi::from_palm(&palm);
        assert!(roi.up.x > 0.7, "fingers point right: {roi:?}");
        let t = std::time::Instant::now();
        let hand = models.landmarks(&image, &roi).unwrap();
        let read = t.elapsed();
        eprintln!("detect {detect:?}, landmarks {read:?}; {hand:?}");
        assert!(hand.confidence > 0.8, "{}", hand.confidence);
        assert_eq!(hand.handedness, Handedness::Left);
        let p = hand.image;
        // Fingertips right of their bases, the middle finger's tip far
        // right in the picture (about x = 290 of 320), the thumb's lowest.
        for tip in [8, 12, 16, 20] {
            assert!(p[tip].x > p[tip - 3].x + 15.0, "finger {tip}: {p:?}");
        }
        assert!(p[12].x > 260.0 && p[12].x < 310.0, "{:?}", p[12]);
        assert!(p[4].y > p[8].y && p[4].y > 170.0, "thumb tip {:?}", p[4]);
        assert!(p[0].x < 100.0, "wrist {:?}", p[0]);
        // Metric: a hand is about 18 cm, wrist to the middle finger's tip.
        let span = (hand.world[12] - hand.world[0]).length();
        assert!(span > 0.12 && span < 0.25, "{span}");
        // The back of the hand faces the camera: a left hand's palm (the
        // cross of forward and thumbwards) points away from it (+z).
        let w = hand.world;
        let palm = (w[9] - w[0]).cross(w[5] - w[17]);
        assert!(palm.normalize().z > 0.5, "{palm}");
        // Followed into the next frame without the detector.
        let mut tracker = HandTracker::default();
        assert!(tracker.track(&models, &image).unwrap().is_some());
        assert!(tracker.is_tracking());
        let again = tracker.track(&models, &image).unwrap().unwrap();
        assert!((again.image[12] - p[12]).length() < 12.0);
    }
}
