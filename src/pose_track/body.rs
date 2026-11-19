//! Body tracking with MediaPipe's BlazePose models (Apache-2.0, as ONNX
//! from OpenCV's model zoo): a detector finds a person in the whole
//! picture (it boxes the face, and gives points for the hips' middle and
//! for the body's extent and lean), then the full-body landmark model
//! reads 33 points from a square crop turned so the body stands upright in
//! it. While the landmarks keep coming, the next frame's crop is taken
//! from them (two extra points the model gives for this) and the detector
//! is skipped.
//!
//! Hands are read again with the hand landmark model ([`super::hand`]),
//! from crops at the wrists the body landmarks give, for the fingers.
//!
//! Landmarks are numbered as BlazePose numbers them: 0 the nose, 1-3 the
//! left eye (inner, middle, outer), 4-6 the right, 7-8 the left and right
//! ears, 9-10 the mouth's left and right corners, then left before right:
//! 11-12 shoulders, 13-14 elbows, 15-16 wrists, 17-18 the little fingers'
//! knuckles, 19-20 the index fingers', 21-22 the thumbs, 23-24 hips, 25-26
//! knees, 27-28 ankles, 29-30 heels, 31-32 the feet's tips. Left and right
//! are the person's own.

use std::path::Path;

use glam::{Vec2, Vec3};
use image::RgbImage;
use tract_onnx::prelude::*;

use super::crop::{Layout, Roi, crop, sigmoid};
use super::hand::{HAND_CONFIDENCE, HandLandmarks, HandModels, Handedness, Plan, plan};

/// How many landmarks a body has.
pub const LANDMARKS: usize = 33;
/// How many the model gives: the body's, then six of its own (two for the
/// next frame's crop).
const MODEL_LANDMARKS: usize = 39;

/// The detector's input side, in pixels.
const DETECT_SIZE: usize = 224;
/// The landmark model's.
const BODY_SIZE: usize = 256;
/// A person counts from this score.
const PERSON_SCORE: f32 = 0.5;
/// Detections overlapping more than this are one.
const PERSON_OVERLAP: f32 = 0.3;
/// The landmarks count (and the body stays tracked) from this confidence.
pub const BODY_CONFIDENCE: f32 = 0.5;
/// A landmark counts as seen from this visibility.
pub const VISIBLE: f32 = 0.5;
/// How much the crop is grown around the alignment points (MediaPipe's).
const BODY_SCALE: f32 = 1.25;
/// A hand's crop from the wrist and knuckles: twice the wrist's distance
/// to the middle knuckle, grown by this.
const HAND_FROM_BODY: f32 = 1.6;

/// The models' files in their folder.
pub const DETECT_FILE: &str = "pose_detection.onnx";
pub const LANDMARK_FILE: &str = "pose_landmark.onnx";

/// The wrist, little finger and index finger knuckles of each side.
const HAND_POINTS: [[usize; 3]; 2] = [[15, 17, 19], [16, 18, 20]];

/// A person the detector found, in the picture's pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Person {
    pub score: f32,
    /// The face's box (top left, bottom right).
    pub min: Vec2,
    pub max: Vec2,
    /// The hips' middle, a point as far from it as the body reaches
    /// (towards the head), the shoulders' middle, and a point for the
    /// upper body's extent.
    pub points: [Vec2; 4],
}

impl Roi {
    /// The crop around a person the detector found.
    pub fn from_person(person: &Person) -> Option<Self> {
        Self::aligned(person.points[0], person.points[1], BODY_SCALE)
    }
}

/// One body read from a picture.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyLandmarks {
    /// In the picture: x and y in pixels, z the depth from the hips (in
    /// the same scale, smaller nearer the camera).
    pub image: [Vec3; LANDMARKS],
    /// In metres round the hips' middle, the picture's axes (x right, y
    /// down, z away from the camera).
    pub world: [Vec3; LANDMARKS],
    /// How likely each landmark is in the picture and not hidden (0..1).
    pub visibility: [f32; LANDMARKS],
    /// How sure the model is that it's a body at all.
    pub confidence: f32,
    /// The crop it was read from.
    pub roi: Roi,
    /// The crop for the next frame.
    pub next: Option<Roi>,
    /// The hands read again at the wrists (the left, the right: the
    /// body's own), if asked for and found.
    pub hands: [Option<HandLandmarks>; 2],
}

impl BodyLandmarks {
    /// The hand of `side`, if it was read.
    #[cfg(test)]
    pub fn hand(&self, side: Handedness) -> Option<&HandLandmarks> {
        self.hands[side_index(side)].as_ref()
    }
}

fn side_index(side: Handedness) -> usize {
    match side {
        Handedness::Left => 0,
        Handedness::Right => 1,
    }
}

/// The two models, ready to run.
pub struct BodyModels {
    detect: Plan,
    landmarks: Plan,
    anchors: Vec<Vec2>,
}

impl BodyModels {
    /// Load the models from `dir`.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let detect = plan(&dir.join(DETECT_FILE), [1, 3, DETECT_SIZE, DETECT_SIZE])
            .map_err(|e| format!("Couldn't load the pose detector: {e}"))?;
        let landmarks = plan(&dir.join(LANDMARK_FILE), [1, BODY_SIZE, BODY_SIZE, 3])
            .map_err(|e| format!("Couldn't load the pose landmark model: {e}"))?;
        Ok(Self {
            detect,
            landmarks,
            anchors: person_anchors(),
        })
    }

    /// The people in `image`, best first.
    pub fn detect(&self, image: &RgbImage) -> Result<Vec<Person>, String> {
        let square = Roi::whole(image.width() as f32, image.height() as f32);
        let input = crop(image, &square, DETECT_SIZE, Layout::NchwSigned);
        let out = self
            .detect
            .run(tvec!(input.into_tvalue()))
            .map_err(|e| e.to_string())?;
        let mut boxes = None;
        let mut scores = None;
        for t in &out {
            let data = values(t)?;
            match t.shape().last() {
                Some(12) => boxes = Some(data),
                Some(1) => scores = Some(data),
                _ => {}
            }
        }
        let (Some(boxes), Some(scores)) = (boxes, scores) else {
            return Err("The pose detector gave unexpected outputs".into());
        };
        if scores.len() != self.anchors.len() || boxes.len() != self.anchors.len() * 12 {
            return Err("The pose detector's outputs don't match its anchors".into());
        }
        let n = DETECT_SIZE as f32;
        let mut people = Vec::new();
        for (i, (&raw, anchor)) in scores.iter().zip(&self.anchors).enumerate() {
            let score = sigmoid(raw);
            if score < PERSON_SCORE {
                continue;
            }
            let r = &boxes[i * 12..(i + 1) * 12];
            let at = |dx: f32, dy: f32| {
                let p = *anchor + Vec2::new(dx, dy) / n;
                square.to_image(p.x, p.y)
            };
            let center = at(r[0], r[1]);
            let half = Vec2::new(r[2], r[3]) / n * square.size * 0.5;
            people.push(Person {
                score,
                min: center - half,
                max: center + half,
                points: std::array::from_fn(|k| at(r[4 + 2 * k], r[5 + 2 * k])),
            });
        }
        people.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut kept: Vec<Person> = Vec::new();
        for person in people {
            if kept.iter().all(|k| overlap(k, &person) <= PERSON_OVERLAP) {
                kept.push(person);
            }
        }
        Ok(kept)
    }

    /// Read a body's landmarks from `roi` of `image` (no hands).
    pub fn landmarks(&self, image: &RgbImage, roi: &Roi) -> Result<BodyLandmarks, String> {
        let input = crop(image, roi, BODY_SIZE, Layout::Nhwc);
        let out = self
            .landmarks
            .run(tvec!(input.into_tvalue()))
            .map_err(|e| e.to_string())?;
        // The landmarks in the crop (x, y, z, visibility, presence), how
        // sure it is there's a body, a mask and heat maps (unused), the
        // landmarks in metres.
        let (mut points, mut confidence, mut world) = (None, None, None);
        for t in &out {
            match t.len() {
                n if n == MODEL_LANDMARKS * 5 => points = Some(values(t)?),
                n if n == MODEL_LANDMARKS * 3 => world = Some(values(t)?),
                1 => confidence = Some(values(t)?[0]),
                _ => {}
            }
        }
        let (Some(points), Some(confidence), Some(world)) = (points, confidence, world) else {
            return Err("The pose landmark model gave unexpected outputs".into());
        };
        let (right, down) = roi.axes();
        let n = BODY_SIZE as f32;
        let at = |i: usize| {
            let p = roi.to_image(points[i * 5] / n, points[i * 5 + 1] / n);
            p.extend(points[i * 5 + 2] / n * roi.size)
        };
        let image_points: [Vec3; LANDMARKS] = std::array::from_fn(at);
        let world_points = std::array::from_fn(|i| {
            let xy = right * world[i * 3] + down * world[i * 3 + 1];
            xy.extend(world[i * 3 + 2])
        });
        let visibility = std::array::from_fn(|i| sigmoid(points[i * 5 + 3]));
        // The crop for the next frame, from the model's own alignment
        // points (the hips' middle and the body's extent).
        let next = Roi::aligned(at(33).truncate(), at(34).truncate(), BODY_SCALE);
        Ok(BodyLandmarks {
            image: image_points,
            world: world_points,
            visibility,
            confidence,
            roi: *roi,
            next,
            hands: [None, None],
        })
    }

    /// Every body in a single picture (a photo), the biggest first: each
    /// person the detector finds, read from its crop and again from the
    /// crop that gave (the surer of the two kept). The detector looks for
    /// faces, so it misses most drawn ones: finding none, the whole
    /// picture is read as one body.
    pub fn read_photo(&self, image: &RgbImage) -> Result<Vec<BodyLandmarks>, String> {
        let mut rois: Vec<Roi> = (self.detect(image)?.iter())
            .filter_map(Roi::from_person)
            .collect();
        if rois.is_empty() {
            let (w, h) = (image.width() as f32, image.height() as f32);
            rois.push(Roi {
                center: Vec2::new(w, h) * 0.5,
                size: w.max(h),
                up: Vec2::NEG_Y,
            });
        }
        let mut bodies: Vec<BodyLandmarks> = Vec::new();
        for roi in rois {
            let mut body = self.landmarks(image, &roi)?;
            if let Some(next) = body.next {
                let again = self.landmarks(image, &next)?;
                if again.confidence >= body.confidence {
                    body = again;
                }
            }
            // Two crops of the same body find it twice.
            if body.confidence >= BODY_CONFIDENCE
                && bodies
                    .iter()
                    .all(|b| hips(b).distance(hips(&body)) > 0.25 * height(b))
            {
                bodies.push(body);
            }
        }
        bodies.sort_by(|a, b| height(b).total_cmp(&height(a)));
        Ok(bodies)
    }
}

/// A body's hips' middle in the picture.
pub fn hips(body: &BodyLandmarks) -> Vec2 {
    (body.image[23] + body.image[24]).truncate() * 0.5
}

/// How tall a body's torso looks in the picture (shoulders to hips), in
/// pixels.
fn height(body: &BodyLandmarks) -> f32 {
    ((body.image[11] + body.image[12]).truncate() * 0.5).distance(hips(body))
}

/// The square a hand of the body is read from, from its wrist and
/// knuckles (MediaPipe's: centred on the middle knuckle, which is taken a
/// third of the way from the index knuckle to the little finger's, turned
/// so the fingers point up). None if the wrist isn't seen.
pub fn hand_roi(body: &BodyLandmarks, side: Handedness) -> Option<Roi> {
    let [w, p, i] = HAND_POINTS[side_index(side)];
    if body.visibility[w] < VISIBLE {
        return None;
    }
    let wrist = body.image[w].truncate();
    let middle = (body.image[i].truncate() * 2.0 + body.image[p].truncate()) / 3.0;
    let up = (middle - wrist).try_normalize()?;
    let size = 2.0 * (middle - wrist).length() * HAND_FROM_BODY;
    (size.is_finite() && size > 4.0).then_some(Roi {
        center: middle + up * size * 0.05,
        size,
        up,
    })
}

/// Follows one body from frame to frame (the detector runs only when
/// there's none to follow), and its hands when asked to.
#[derive(Default)]
pub struct BodyTracker {
    roi: Option<Roi>,
    hands: [Option<Roi>; 2],
}

impl BodyTracker {
    /// The body in `image`, if any (sure enough).
    pub fn track(
        &mut self,
        models: &BodyModels,
        image: &RgbImage,
    ) -> Result<Option<BodyLandmarks>, String> {
        if let Some(roi) = self.roi {
            let body = models.landmarks(image, &roi)?;
            if body.confidence >= BODY_CONFIDENCE {
                self.roi = body.next;
                return Ok(Some(body));
            }
            self.reset();
        }
        let Some(roi) = models.detect(image)?.first().and_then(Roi::from_person) else {
            return Ok(None);
        };
        let body = models.landmarks(image, &roi)?;
        if body.confidence < BODY_CONFIDENCE {
            return Ok(None);
        }
        self.roi = body.next;
        Ok(Some(body))
    }

    /// Read `body`'s hands with the hand model: from where they were last
    /// frame if that's still at the wrist, else from the wrist and
    /// knuckles (then once more from the landmarks found, for a better
    /// crop).
    pub fn track_hands(
        &mut self,
        models: &HandModels,
        image: &RgbImage,
        body: &mut BodyLandmarks,
    ) -> Result<(), String> {
        for side in [Handedness::Left, Handedness::Right] {
            let k = side_index(side);
            let Some(at_wrist) = hand_roi(body, side) else {
                self.hands[k] = None;
                continue;
            };
            let followed = self.hands[k]
                .filter(|roi| roi.center.distance(at_wrist.center) < at_wrist.size * 0.5);
            let mut hand = models.landmarks(image, &followed.unwrap_or(at_wrist))?;
            if followed.is_none() && hand.confidence >= HAND_CONFIDENCE {
                let again = models.landmarks(image, &Roi::from_landmarks(&hand.image))?;
                if again.confidence >= hand.confidence {
                    hand = again;
                }
            } else if followed.is_some() && hand.confidence < HAND_CONFIDENCE {
                hand = models.landmarks(image, &at_wrist)?;
            }
            if hand.confidence < HAND_CONFIDENCE {
                self.hands[k] = None;
                continue;
            }
            self.hands[k] = Some(Roi::from_landmarks(&hand.image));
            body.hands[k] = Some(hand);
        }
        Ok(())
    }

    /// Whether a body is being followed (the detector is skipped).
    #[cfg(test)]
    pub fn is_tracking(&self) -> bool {
        self.roi.is_some()
    }

    /// Forget the body (a new camera, a jump).
    pub fn reset(&mut self) {
        self.roi = None;
        self.hands = [None, None];
    }
}

/// A tensor's values.
fn values(t: &TValue) -> Result<Vec<f32>, String> {
    Ok((t.to_plain_array_view::<f32>())
        .map_err(|e| e.to_string())?
        .iter()
        .copied()
        .collect())
}

/// How much two people's boxes overlap (intersection over union).
fn overlap(a: &Person, b: &Person) -> f32 {
    let lo = a.min.max(b.min);
    let hi = a.max.min(b.max);
    let inter = (hi - lo).max(Vec2::ZERO);
    let inter = inter.x * inter.y;
    let area = |p: &Person| {
        let s = (p.max - p.min).max(Vec2::ZERO);
        s.x * s.y
    };
    inter / (area(a) + area(b) - inter).max(1e-6)
}

/// The detector's anchors (SSD, MediaPipe's options for it: five layers
/// of strides 8, 16, 32, 32, 32, two anchors a layer, layers of a stride
/// sharing a grid): two a cell on a 28×28 grid, two on a 14×14 one, six
/// on a 7×7 one, cell centres in 0..1.
fn person_anchors() -> Vec<Vec2> {
    let mut out = Vec::with_capacity(2254);
    for (grid, per_cell) in [
        (DETECT_SIZE / 8, 2),
        (DETECT_SIZE / 16, 2),
        (DETECT_SIZE / 32, 6),
    ] {
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

    fn dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/pose-models/v1")
    }

    fn models() -> Option<BodyModels> {
        if !super::super::is_installed(&dir()) {
            eprintln!("the pose models aren't pulled from Git LFS; skipping");
            return None;
        }
        Some(BodyModels::load(&dir()).unwrap())
    }

    pub(crate) fn photo() -> RgbImage {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/body-photo.jpg");
        image::open(path).unwrap().to_rgb8()
    }

    #[test]
    fn anchors_match_the_detector() {
        let a = person_anchors();
        assert_eq!(a.len(), 2254);
        assert!((a[0] - Vec2::splat(0.5 / 28.0)).length() < 1e-6);
        assert!((a[1568] - Vec2::splat(0.5 / 14.0)).length() < 1e-6);
        assert!((a[2253] - Vec2::splat(6.5 / 7.0)).length() < 1e-6);
    }

    #[test]
    fn the_models_find_the_man_in_the_doorway() {
        let Some(models) = models() else {
            return;
        };
        let image = photo();
        let t = std::time::Instant::now();
        let people = models.detect(&image).unwrap();
        let detect = t.elapsed();
        assert!(!people.is_empty(), "no person found");
        let roi = Roi::from_person(&people[0]).unwrap();
        // Upright, round the hips (about (160, 300) of 320×479).
        assert!(roi.up.y < -0.95, "{roi:?}");
        assert!(
            roi.center.distance(Vec2::new(160.0, 300.0)) < 40.0,
            "{roi:?}"
        );
        let t = std::time::Instant::now();
        let body = models.landmarks(&image, &roi).unwrap();
        let read = t.elapsed();
        eprintln!("detect {detect:?}, landmarks {read:?}");
        assert!(body.confidence > 0.9, "{}", body.confidence);
        let p = body.image;
        // Facing the camera: his left on the picture's right.
        assert!(p[11].x > p[12].x + 50.0 && p[23].x > p[24].x + 25.0);
        // Head, shoulders, hips, knees, ankles, top to bottom.
        for side in 0..2 {
            let chain = [0, 11 + side, 23 + side, 25 + side, 27 + side];
            for w in chain.windows(2) {
                assert!(p[w[0]].y + 25.0 < p[w[1]].y, "{w:?}: {p:?}");
            }
        }
        // Hands on hips: elbows out past the shoulders, wrists in from
        // the elbows at the waist.
        assert!(p[13].x > p[11].x + 5.0 && p[14].x < p[12].x - 5.0, "{p:?}");
        assert!(p[15].x < p[13].x && p[16].x > p[14].x, "{p:?}");
        for w in [15, 16] {
            assert!(p[w].y > p[11].y + 20.0 && p[w].y < p[25].y, "{p:?}");
        }
        for i in [0, 11, 12, 13, 14, 23, 24] {
            assert!(body.visibility[i] > 0.8, "{i}: {:?}", body.visibility);
        }
        // Metric: shoulders a third of a metre apart, the nose well above
        // the hips (the origin), the body facing the camera (-z).
        let w = body.world;
        let shoulders = w[11].distance(w[12]);
        assert!(shoulders > 0.25 && shoulders < 0.5, "{shoulders}");
        assert!(w[0].y < -0.4 && w[0].y > -0.9, "{}", w[0]);
        assert!(((w[23] + w[24]) * 0.5).length() < 0.05);
        let up = (w[11] + w[12] - w[23] - w[24]) * -0.5;
        let facing = (w[11] - w[12]).cross(-up).normalize();
        assert!(facing.z < -0.8, "{facing}");
        // Followed into the next frame without the detector.
        let mut tracker = BodyTracker::default();
        let first = tracker.track(&models, &image).unwrap().unwrap();
        assert!(tracker.is_tracking());
        let again = tracker.track(&models, &image).unwrap().unwrap();
        for i in [0, 11, 12, 23, 24] {
            let moved = again.image[i]
                .truncate()
                .distance(first.image[i].truncate());
            assert!(moved < 12.0, "{i}: {moved}");
        }
    }

    /// The body read from the photo, its left wrist and knuckles moved
    /// onto the left hand in the hand photo, its right wrist hidden.
    fn body_on_the_hand_photo(models: &BodyModels) -> BodyLandmarks {
        let image = photo();
        let mut body = models
            .landmarks(
                &image,
                &Roi::from_person(&models.detect(&image).unwrap()[0]).unwrap(),
            )
            .unwrap();
        body.image[15] = Vec3::new(46.0, 88.0, 0.0);
        body.image[17] = Vec3::new(153.0, 48.0, 0.0);
        body.image[19] = Vec3::new(167.0, 136.0, 0.0);
        body.visibility[15] = 0.99;
        body.visibility[16] = 0.1;
        body
    }

    #[test]
    fn the_hands_are_read_at_the_wrists() {
        let Some(models) = models() else {
            return;
        };
        let hands = HandModels::load(&dir()).unwrap();
        let image =
            image::open(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/hand-photo.jpg"))
                .unwrap()
                .to_rgb8();
        let mut body = body_on_the_hand_photo(&models);
        let roi = hand_roi(&body, Handedness::Left).unwrap();
        // Fingers to the right.
        assert!(roi.up.x > 0.9, "{roi:?}");
        assert!(hand_roi(&body, Handedness::Right).is_none());
        let mut tracker = BodyTracker::default();
        let t = std::time::Instant::now();
        tracker.track_hands(&hands, &image, &mut body).unwrap();
        eprintln!("both passes {:?}", t.elapsed());
        assert!(body.hand(Handedness::Right).is_none());
        let hand = body.hand(Handedness::Left).expect("the left hand is read");
        assert!(hand.confidence > 0.8, "{}", hand.confidence);
        // Where the hand model finds it from its own detector: the wrist
        // about (47, 88), the middle finger's tip about (294, 119).
        assert!(
            hand.image[0].truncate().distance(Vec2::new(47.0, 88.0)) < 15.0,
            "{:?}",
            hand.image[0]
        );
        assert!(
            hand.image[12].truncate().distance(Vec2::new(294.0, 119.0)) < 15.0,
            "{:?}",
            hand.image[12]
        );
        // Followed: next frame from its own landmarks, in one pass.
        let mut again = body_on_the_hand_photo(&models);
        tracker.track_hands(&hands, &image, &mut again).unwrap();
        let next = again.hand(Handedness::Left).unwrap();
        assert!(next.image[12].distance(hand.image[12]) < 10.0);
    }
}
