//! Posing figures from the camera and from photos. The camera window has
//! two modes: Hand (the Hand Scanner: a figure's hand posed from yours)
//! and Body (Pose from Camera: the whole figure posed from you, hands
//! included if wanted). The camera's frames go to the tracker
//! (`pose_track`, on its own thread); what it finds is retargeted onto the
//! selected figure (`canvas::figure::retarget`). "Capture" applies one
//! frame's pose (one undo step); "Live" drives the figure every frame,
//! smoothed, as one undo step until it's turned off. Pose from Photo reads
//! a body from a picture once, shows it over the picture, and applies it
//! as one undo step. Everything runs on the device: no frame or photo is
//! saved or sent anywhere.
//!
//! Hand and camera share one window rather than two: they share the
//! camera, its permission, the camera choice and the tracker thread, and
//! switching between them keeps the camera running.
//!
//! Posing from landmarks needs no camera, so that part is in every build;
//! the camera and the tracker come with the `pose-capture` feature.
// (Without them nothing calls the posing but its tests.)
#![cfg_attr(not(feature = "pose-capture"), allow(dead_code))]

use std::time::Instant;

use glam::Vec3;

use crate::PainterApp;
use crate::canvas::figure::presets::is_finger;
use crate::canvas::figure::retarget::{self, BodyPoints, HAND_LANDMARKS, OneEuro};
use crate::canvas::figure::skeleton::{HumanJoint, Side};
use crate::canvas::storage::LayerId;

/// Which of the figure's hands the scanner poses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TargetHand {
    /// The one the camera sees (the other if the figure has only that).
    #[default]
    Auto,
    Left,
    Right,
}

/// What the camera window poses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanMode {
    /// A hand (the Hand Scanner).
    #[default]
    Hand,
    /// The whole body (Pose from Camera).
    Body,
}

/// A hand the camera saw: its landmarks in metres, in a right-handed
/// space (the camera's: x right, y down, z away), and which hand it is.
#[derive(Clone, Debug, PartialEq)]
pub struct SeenHand {
    pub points: [Vec3; HAND_LANDMARKS],
    pub side: Side,
    /// When it was seen, in seconds (for smoothing Live).
    pub time: f64,
}

/// A body the camera (or a photo) showed: its landmarks in the camera's
/// axes, and its hands' (the left, the right) if they were read.
#[derive(Clone, Debug, PartialEq)]
pub struct SeenBody {
    pub body: BodyPoints,
    pub hands: [Option<[Vec3; HAND_LANDMARKS]>; 2],
    /// When it was seen, in seconds (for smoothing Live).
    pub time: f64,
}

#[cfg(feature = "pose-capture")]
impl SeenBody {
    fn from_landmarks(body: &crate::pose_track::BodyLandmarks, time: f64) -> Self {
        Self {
            body: BodyPoints {
                points: body.world,
                visibility: body.visibility,
            },
            hands: [0, 1].map(|k| body.hands[k].as_ref().map(|h| h.world)),
            time,
        }
    }
}

pub struct HandScanner {
    pub open: bool,
    pub mode: ScanMode,
    pub target: TargetHand,
    /// Driving the figure every frame.
    pub live: bool,
    /// The preview shown as in a mirror (and in Body mode, the figure
    /// posed as your mirror image).
    pub mirror: bool,
    /// Body mode: the hands read too (fingers and wrists).
    pub include_hands: bool,
    /// Body mode and photos: the whole figure turned as the body is
    /// (else it keeps facing as it does).
    pub turn_body: bool,
    /// Photos: each figure placed where its body stands to the camera.
    pub place_body: bool,
    /// The camera last chosen, by name (kept in the settings).
    pub camera_name: Option<String>,
    filter: OneEuro,
    body_filter: OneEuro,
    hand_filters: [OneEuro; 2],
    clock: Instant,
    /// The layer and hand Live is driving (a change starts over).
    driving: Option<(LayerId, Side)>,
    /// The layer Live is driving in Body mode.
    driving_body: Option<LayerId>,
    /// The camera permission was asked for since the window opened (the
    /// system's prompt takes the focus, which stops the camera: asking
    /// again on coming back would ask for ever).
    pub asked_permission: bool,
    #[cfg(feature = "pose-capture")]
    pub session: Option<session::ScanSession>,
    /// Pose from Photo's window, while it's open.
    #[cfg(feature = "pose-capture")]
    pub photo: Option<session::PhotoPose>,
}

impl Default for HandScanner {
    fn default() -> Self {
        Self {
            open: false,
            mode: ScanMode::Hand,
            target: TargetHand::Auto,
            live: false,
            mirror: true,
            include_hands: true,
            turn_body: true,
            place_body: true,
            camera_name: None,
            filter: OneEuro::for_hands(),
            body_filter: OneEuro::for_bodies(),
            hand_filters: [OneEuro::for_hands(), OneEuro::for_hands()],
            clock: Instant::now(),
            driving: None,
            driving_body: None,
            asked_permission: false,
            #[cfg(feature = "pose-capture")]
            session: None,
            #[cfg(feature = "pose-capture")]
            photo: None,
        }
    }
}

impl HandScanner {
    /// A new document: Live stops driving the old one's figure (its undo
    /// step went with it).
    pub fn forget_document(&mut self) {
        self.live = false;
        self.reset_filters();
    }

    fn reset_filters(&mut self) {
        self.driving = None;
        self.driving_body = None;
        self.filter.reset();
        self.body_filter.reset();
        for f in &mut self.hand_filters {
            f.reset();
        }
    }
}

impl PainterApp {
    /// The figure the camera poses (its index in the scene): the selected
    /// one, or the only one.
    fn scanner_figure(&self) -> Option<usize> {
        let layer = self.active_figure()?;
        self.figure_selected_instance()
            .or((layer.scene.len() == 1).then_some(0))
    }

    /// The figure (its index in the scene) and its hand a hand `seen` the
    /// camera poses, as the scanner's target says.
    fn scanner_target(&self, seen: Side) -> Option<(usize, Side)> {
        let f = self.scanner_figure()?;
        let model = self.active_figure()?.scene.get(f)?.model.resolve().ok()?;
        let has = |side: Side| model.skeleton.find(HumanJoint::Hand(side)).is_some();
        let side = match self.workspace.figure.scanner.target {
            TargetHand::Left => Side::Left,
            TargetHand::Right => Side::Right,
            TargetHand::Auto if has(seen) => seen,
            TargetHand::Auto => seen.mirror(),
        };
        has(side).then_some((f, side))
    }

    /// Pose the target hand as `hand` shows: `live`, smoothed and joined to
    /// the Live gesture's undo step; else as one step of its own. Whether
    /// a figure took it.
    pub(crate) fn scanner_apply(&mut self, hand: &SeenHand, live: bool) -> bool {
        let Some((f, side)) = self.scanner_target(hand.side) else {
            return false;
        };
        let Some(model) = (self.active_figure())
            .and_then(|l| l.scene.get(f))
            .and_then(|i| i.model.resolve().ok())
        else {
            return false;
        };
        // A left hand seen poses a right one as a mirror would.
        let mut points = if side == hand.side {
            hand.points
        } else {
            retarget::mirror_landmarks(&hand.points)
        };
        let scanner = &mut self.workspace.figure.scanner;
        if live {
            let layer = self.canvas.layers[self.canvas.active_layer_idx].id;
            if scanner.driving != Some((layer, side)) {
                scanner.driving = Some((layer, side));
                scanner.filter.reset();
            }
            points = scanner.filter.filter_points(hand.time, &points);
        }
        let Some(posed) = retarget::retarget_hand(&model.skeleton, side, &points) else {
            return false;
        };
        let on_hand = move |h: HumanJoint| {
            is_finger(h) && matches!(h, HumanJoint::Finger(s, ..) if s == side)
        };
        let edit = |layer: &mut crate::canvas::figure::FigureLayer| {
            if let Some(instance) = layer.scene.get_mut(f) {
                posed
                    .pose
                    .apply(&model.skeleton, &mut instance.pose, on_hand, false);
            }
        };
        if live {
            self.figure_edit("Hand Scanner", edit);
        } else {
            self.figure_step("Hand Scanner", edit);
        }
        true
    }

    /// Pose the figure as `seen` shows (`mirror`ed: as your mirror image):
    /// `live`, smoothed and joined to the Live gesture's undo step; else as
    /// one step of its own, `label`. Joints of limbs not seen keep their
    /// pose; the hands' fingers and wrists follow the hands read if the
    /// scanner includes them; the whole figure turns as the body does if
    /// it says so. Whether a figure took it.
    pub(crate) fn body_apply(
        &mut self,
        seen: &SeenBody,
        live: bool,
        mirror: bool,
        label: &str,
    ) -> bool {
        let Some(f) = self.scanner_figure() else {
            return false;
        };
        let Some(model) = (self.active_figure())
            .and_then(|l| l.scene.get(f))
            .and_then(|i| i.model.resolve().ok())
        else {
            return false;
        };
        let (mut body, mut hands) = (seen.body.clone(), seen.hands);
        if mirror {
            body = body.mirrored();
            let mirrored =
                |h: Option<[Vec3; HAND_LANDMARKS]>| h.map(|p| retarget::mirror_landmarks(&p));
            hands = [mirrored(hands[1]), mirrored(hands[0])];
        }
        let scanner = &mut self.workspace.figure.scanner;
        if !scanner.include_hands {
            hands = [None, None];
        }
        if live {
            let layer = self.canvas.layers[self.canvas.active_layer_idx].id;
            if scanner.driving_body != Some(layer) {
                scanner.reset_filters();
                scanner.driving_body = Some(layer);
            }
            body.points = scanner.body_filter.filter_points(seen.time, &body.points);
            for (hand, filter) in hands.iter_mut().zip(&mut scanner.hand_filters) {
                match hand {
                    Some(points) => *points = filter.filter_points(seen.time, points),
                    // Found again later: from where it is then.
                    None => filter.reset(),
                }
            }
        }
        let turn_body = scanner.turn_body;
        let Some(mut posed) = retarget::retarget_body(
            &model.skeleton,
            &body,
            [hands[0].as_ref(), hands[1].as_ref()],
        ) else {
            return false;
        };
        // Turned as you are to the camera, it's turned so to the figure's
        // camera (the hips' turn is read as from facing it head on).
        if turn_body
            && let Some(hips) = posed.pose.get(HumanJoint::Hips)
            && let Some(layer) = self.active_figure()
            && let Some(instance) = layer.scene.get(f)
        {
            let root = glam::Quat::from_array(instance.rotation).normalize();
            let facing = root.inverse() * glam::Quat::from_rotation_y(layer.camera.yaw);
            posed.pose.set(HumanJoint::Hips, facing * hips);
        }
        let which = move |h: HumanJoint| turn_body || h != HumanJoint::Hips;
        let edit = |layer: &mut crate::canvas::figure::FigureLayer| {
            if let Some(instance) = layer.scene.get_mut(f) {
                posed
                    .pose
                    .apply(&model.skeleton, &mut instance.pose, which, false);
            }
        };
        if live {
            self.figure_edit(label, edit);
        } else {
            self.figure_step(label, edit);
        }
        true
    }

    /// Turn Live on or off; off ends its undo step.
    pub(crate) fn scanner_set_live(&mut self, on: bool) {
        let scanner = &mut self.workspace.figure.scanner;
        let was = std::mem::replace(&mut scanner.live, on);
        scanner.reset_filters();
        if was && !on {
            self.figure_edit_done();
        }
    }

    /// Pose a hand or the body (Live ends: a new undo step).
    pub(crate) fn scanner_set_mode(&mut self, mode: ScanMode) {
        if self.workspace.figure.scanner.mode != mode {
            self.scanner_set_live(false);
            self.workspace.figure.scanner.mode = mode;
        }
    }

    /// Open the Hand Scanner (from the Hand menu or its shortcut).
    pub(crate) fn open_hand_scanner(&mut self) {
        self.scanner_set_mode(ScanMode::Hand);
        self.workspace.figure.scanner.open = true;
    }

    /// Open Pose from Camera (the Pose menu): the same window, posing the
    /// whole body.
    pub(crate) fn open_pose_from_camera(&mut self) {
        self.scanner_set_mode(ScanMode::Body);
        self.workspace.figure.scanner.open = true;
    }

    /// Close it: the camera stops, Live ends.
    pub(crate) fn close_hand_scanner(&mut self) {
        self.scanner_set_live(false);
        let scanner = &mut self.workspace.figure.scanner;
        scanner.open = false;
        scanner.asked_permission = false;
        #[cfg(feature = "pose-capture")]
        {
            scanner.session = None;
        }
    }

    /// Pose from Photo (the Pose menu): pick a picture; its body is read
    /// and shown over it to apply.
    pub(crate) fn pose_from_photo(&mut self) {
        self.pick_open(crate::app::files::OpenFor::PosePhoto);
    }

    /// A picture picked for Pose from Photo: read on a job's thread.
    #[cfg(not(feature = "pose-capture"))]
    pub(crate) fn pose_photo_in_background(
        &mut self,
        _name: String,
        _source: crate::app::import::FileSource,
    ) {
        self.report("This build can't read poses (it was built without `pose-capture`)".into());
    }
}

#[cfg(feature = "pose-capture")]
pub mod session {
    //! The camera and the tracker while the camera window is open, and a
    //! photo being posed from.

    use std::sync::Arc;

    use crate::PainterApp;
    use crate::capture::{Camera, CameraError, CameraInfo};
    use crate::pose_track::{BodyLandmarks, Frame, Handedness, PoseWorker, TrackMode, Tracked};

    use super::{ScanMode, SeenBody, SeenHand};
    use crate::canvas::figure::skeleton::Side;

    /// The frame size asked of the camera.
    const CAMERA_SIZE: [u32; 2] = [640, 480];
    /// A photo's longest side is brought down to this before it's read
    /// (the models read 256-pixel crops: more is only slower).
    const PHOTO_SIDE: u32 = 1280;

    pub struct ScanSession {
        pub camera: Box<dyn Camera>,
        pub cameras: Vec<CameraInfo>,
        /// The camera running (of `cameras`).
        pub index: Option<usize>,
        /// The camera wanted (started once the camera is allowed).
        pub wanted: usize,
        worker: PoseWorker,
        /// The newest frame, for the preview.
        pub frame: Option<Frame>,
        /// The newest frame the preview has shown, as a texture.
        pub shown: u64,
        pub texture: Option<eframe::egui::TextureHandle>,
        /// The tracker's newest answer.
        pub tracked: Option<Tracked>,
        /// What's wrong, to show.
        pub error: Option<String>,
        /// The app asked for the camera permission (and is waiting).
        pub asked: bool,
    }

    impl ScanSession {
        pub fn new(camera: Box<dyn Camera>) -> Self {
            Self::with_worker(camera, PoseWorker::start())
        }

        pub fn with_worker(camera: Box<dyn Camera>, worker: PoseWorker) -> Self {
            Self {
                camera,
                cameras: Vec::new(),
                index: None,
                wanted: 0,
                worker,
                frame: None,
                shown: 0,
                texture: None,
                tracked: None,
                error: None,
                asked: false,
            }
        }

        /// Start camera `index`.
        pub fn start(&mut self, index: usize) {
            self.frame = None;
            self.tracked = None;
            self.error = None;
            self.wanted = index;
            match self.camera.start(index, CAMERA_SIZE) {
                Ok(()) => self.index = Some(index),
                Err(CameraError::PermissionNeeded) => {
                    self.index = None;
                    if !self.asked {
                        self.asked = true;
                        self.camera.request_permission();
                    }
                }
                Err(err) => {
                    self.index = None;
                    self.error = Some(err.to_string());
                }
            }
        }

        /// Whether frames are coming.
        pub fn running(&self) -> bool {
            self.index.is_some()
        }

        /// Waiting for the user to allow the camera, or refused.
        pub fn needs_permission(&self) -> bool {
            self.index.is_none() && !self.camera.has_permission()
        }
    }

    impl Drop for ScanSession {
        fn drop(&mut self) {
            self.camera.stop();
            retire(self.texture.take());
        }
    }

    impl Drop for PhotoPose {
        fn drop(&mut self) {
            retire(self.texture.take());
        }
    }

    /// Previews of windows closed this frame, freed the next
    /// ([`free_retired`]): egui-wgpu 0.29 frees a texture before it submits
    /// the frame, so one drawn and freed in the same frame panics in
    /// `Queue::submit` ("texture has been destroyed").
    static RETIRED: std::sync::Mutex<Vec<eframe::egui::TextureHandle>> =
        std::sync::Mutex::new(Vec::new());

    fn retire(texture: Option<eframe::egui::TextureHandle>) {
        if let Some(texture) = texture
            && let Ok(mut retired) = RETIRED.lock()
        {
            retired.push(texture);
        }
    }

    /// Free the previews retired last frame (call before drawing any).
    pub fn free_retired() {
        if let Ok(mut retired) = RETIRED.lock() {
            retired.clear();
        }
    }

    /// A photo being posed from: read on a job's thread, then shown with
    /// the body found over it until applied or closed.
    pub struct PhotoPose {
        /// The file's name, for the window.
        pub name: String,
        /// The picture (once read).
        pub image: Option<Arc<image::RgbImage>>,
        /// The bodies found in it, the biggest first (none until read).
        pub bodies: Vec<BodyLandmarks>,
        /// Which of them to pose figures from (the first figure the
        /// selected one, each other a new figure beside it).
        pub chosen: Vec<bool>,
        /// Still being read.
        pub reading: bool,
        pub error: Option<String>,
        pub texture: Option<eframe::egui::TextureHandle>,
    }

    /// Models for a worker in tests: the repository's.
    #[cfg(test)]
    pub fn test_worker() -> Option<PoseWorker> {
        use crate::pose_track::{BodyModels, HandModels};
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pose-models/v1");
        if !crate::pose_track::is_installed(&dir) {
            return None;
        }
        let hands = Arc::new(HandModels::load(&dir).unwrap());
        let bodies = Arc::new(BodyModels::load(&dir).unwrap());
        Some(PoseWorker::with_models(
            move || Ok(hands),
            move || Ok(bodies),
        ))
    }

    fn seen_hand(tracked: &Tracked, time: f64) -> Option<SeenHand> {
        let hand = tracked.hand.as_ref()?;
        Some(SeenHand {
            points: hand.world,
            side: match hand.handedness {
                Handedness::Left => Side::Left,
                Handedness::Right => Side::Right,
            },
            time,
        })
    }

    impl PainterApp {
        /// What the tracker should look for now.
        fn track_mode(&self) -> TrackMode {
            let scanner = &self.workspace.figure.scanner;
            match scanner.mode {
                ScanMode::Hand => TrackMode::Hand,
                ScanMode::Body => TrackMode::Body {
                    hands: scanner.include_hands,
                },
            }
        }

        /// Open the camera window on `camera` (the system's when it opens
        /// on its own; tests give theirs, and their worker).
        pub(crate) fn scanner_begin(
            &mut self,
            camera: Box<dyn Camera>,
            worker: Option<PoseWorker>,
        ) {
            let mut session = match worker {
                Some(worker) => ScanSession::with_worker(camera, worker),
                None => ScanSession::new(camera),
            };
            session.asked = self.workspace.figure.scanner.asked_permission;
            match session.camera.list() {
                Ok(cameras) => session.cameras = cameras,
                Err(err) => session.error = Some(err.to_string()),
            }
            if !session.cameras.is_empty() {
                let scanner = &self.workspace.figure.scanner;
                let index = (scanner.camera_name.as_ref())
                    .and_then(|name| session.cameras.iter().position(|c| &c.name == name))
                    .unwrap_or(0);
                session.start(index);
            }
            let scanner = &mut self.workspace.figure.scanner;
            scanner.asked_permission |= session.asked;
            scanner.session = Some(session);
        }

        /// Switch to camera `index`, and remember it.
        pub(crate) fn scanner_pick_camera(&mut self, index: usize) {
            let scanner = &mut self.workspace.figure.scanner;
            let Some(session) = scanner.session.as_mut() else {
                return;
            };
            if let Some(camera) = session.cameras.get(index) {
                scanner.camera_name = Some(camera.name.clone());
                session.start(index);
            }
        }

        /// One frame of the camera window while it's open: the camera
        /// stops while the app isn't focused (and starts again when it
        /// is), new frames go to the tracker, and with Live on, what it
        /// found poses the figure.
        pub(crate) fn scanner_poll(&mut self, focused: bool) {
            let scanner = &mut self.workspace.figure.scanner;
            if !scanner.open {
                return;
            }
            if !focused {
                if let Some(session) = scanner.session.take() {
                    scanner.asked_permission |= session.asked;
                    drop(session);
                    self.scanner_set_live(false);
                }
                return;
            }
            if scanner.session.is_none() {
                self.scanner_begin(crate::capture::system_camera(), None);
            }
            let mode = self.track_mode();
            let Some(session) = self.workspace.figure.scanner.session.as_mut() else {
                return;
            };
            // Permission granted since it was asked for: start now.
            if session.index.is_none()
                && session.asked
                && session.camera.has_permission()
                && !session.cameras.is_empty()
            {
                session.asked = false;
                session.start(session.wanted);
            }
            self.workspace.figure.scanner.asked_permission |= session.asked;
            if let Some(err) = session.camera.error() {
                session.error = Some(err.to_string());
            }
            session.worker.set_mode(mode);
            if let Some(frame) = session.camera.latest_frame() {
                session.worker.submit(frame.clone());
                session.frame = Some(frame);
            }
            let time = self.workspace.figure.scanner.clock.elapsed().as_secs_f64();
            let (mut hand, mut body) = (None, None);
            match session.worker.take() {
                // (A frame read for what was looked for before is dropped.)
                Some(Ok(tracked)) if tracked.mode == mode => {
                    hand = seen_hand(&tracked, time);
                    body = (tracked.body.as_ref()).map(|b| SeenBody::from_landmarks(b, time));
                    session.tracked = Some(tracked);
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => session.error = Some(err),
                None => {}
            }
            let scanner = &self.workspace.figure.scanner;
            if !scanner.live {
                return;
            }
            let mirror = scanner.mirror;
            if let Some(hand) = hand {
                self.scanner_apply(&hand, true);
            } else if let Some(body) = body {
                self.body_apply(&body, true, mirror, "Pose from Camera");
            }
        }

        /// Capture: the hand or body in the newest frame, on the figure,
        /// as one undo step. Whether there was one to apply.
        pub(crate) fn scanner_capture(&mut self) -> bool {
            let scanner = &self.workspace.figure.scanner;
            let Some(tracked) = scanner.session.as_ref().and_then(|s| s.tracked.clone()) else {
                return false;
            };
            if let Some(hand) = seen_hand(&tracked, 0.0) {
                return self.scanner_apply(&hand, false);
            }
            let mirror = scanner.mirror;
            match &tracked.body {
                Some(body) => self.body_apply(
                    &SeenBody::from_landmarks(body, 0.0),
                    false,
                    mirror,
                    "Pose from Camera",
                ),
                None => false,
            }
        }

        /// A picture picked for Pose from Photo: decoded and read on a
        /// job's thread, its window open meanwhile.
        pub(crate) fn pose_photo_in_background(
            &mut self,
            name: String,
            source: crate::app::import::FileSource,
        ) {
            self.workspace.figure.scanner.photo = Some(PhotoPose {
                name: name.clone(),
                image: None,
                bodies: Vec::new(),
                chosen: Vec::new(),
                reading: true,
                error: None,
                texture: None,
            });
            self.spawn_job(None, move || {
                let result = source
                    .read()
                    .and_then(|bytes| crate::app::import::decode_image(&bytes))
                    .map(|img| Arc::new(photo_rgb(img)))
                    .and_then(|image| {
                        let body = crate::pose_track::read_photo(&image, true)?;
                        Ok((image, body))
                    });
                Box::new(move |app: &mut PainterApp| app.photo_read(&name, result))
            });
        }

        /// A photo's reading ended (if its window is still open for it).
        pub(crate) fn photo_read(
            &mut self,
            name: &str,
            result: Result<(Arc<image::RgbImage>, Vec<BodyLandmarks>), String>,
        ) {
            let Some(photo) = self.workspace.figure.scanner.photo.as_mut() else {
                return;
            };
            if photo.name != name || !photo.reading {
                return;
            }
            photo.reading = false;
            match result {
                Ok((image, bodies)) => {
                    photo.image = Some(image);
                    // The biggest, to start with.
                    photo.chosen = (0..bodies.len()).map(|i| i == 0).collect();
                    photo.bodies = bodies;
                }
                Err(err) => photo.error = Some(err),
            }
        }

        /// Pick or unpick body `i` of the photo.
        pub(crate) fn photo_toggle(&mut self, i: usize) {
            if let Some(chosen) =
                (self.workspace.figure.scanner.photo.as_mut()).and_then(|p| p.chosen.get_mut(i))
            {
                *chosen = !*chosen;
            }
        }

        /// Pose figures as the photo's chosen bodies, as one undo step,
        /// and close its window: the selected figure as the first, a copy
        /// of it beside it for each other one. With Place (always with
        /// several), each stands where its body is to the photo's camera,
        /// seen through the layer's. Whether a figure took it.
        pub(crate) fn photo_apply(&mut self) -> bool {
            let scanner = &self.workspace.figure.scanner;
            let Some(photo) = scanner.photo.as_ref() else {
                return false;
            };
            let Some(image) = photo.image.as_ref() else {
                return false;
            };
            let chosen: Vec<&BodyLandmarks> = (photo.bodies.iter().zip(&photo.chosen))
                .filter_map(|(b, &c)| c.then_some(b))
                .collect();
            let hands = scanner.include_hands;
            let turn = scanner.turn_body;
            let place = scanner.place_body || chosen.len() > 1;
            let (Some(f), Some(layer)) = (self.scanner_figure(), self.active_figure()) else {
                return false;
            };
            let Some(base) = layer.scene.get(f) else {
                return false;
            };
            let Ok(model) = base.model.resolve() else {
                return false;
            };
            let view = PhotoView {
                camera: &layer.camera,
                photo: image.dimensions(),
                canvas: (self.canvas.width(), self.canvas.height()),
            };
            let figures: Vec<_> = (chosen.iter())
                .filter_map(|body| {
                    let mut figure = base.clone();
                    let posed =
                        pose_from_photo(&model, &mut figure, body, &view, hands, turn, place);
                    posed.then_some(figure)
                })
                .collect();
            if figures.is_empty() {
                return false;
            }
            self.figure_step("Pose from Photo", move |layer| {
                let mut figures = figures.into_iter();
                if let (Some(slot), Some(first)) = (layer.scene.get_mut(f), figures.next()) {
                    *slot = first;
                }
                layer.scene.extend(figures);
            });
            self.workspace.figure.scanner.photo = None;
            true
        }
    }

    /// How a photo maps onto the figure layer: fitted in the canvas, seen
    /// through the layer's camera.
    struct PhotoView<'a> {
        camera: &'a crate::canvas::figure::FigureCamera,
        photo: (u32, u32),
        canvas: (usize, usize),
    }

    impl PhotoView<'_> {
        /// Where `body`'s hips are in the scene: on the camera's ray
        /// through them, as far as its size in the photo says (MediaPipe's
        /// metres against its pixels; a lens as the layer's). None if it
        /// shows too little to say.
        fn hips(&self, body: &BodyLandmarks) -> Option<glam::Vec3> {
            use glam::Vec2;
            let (pw, ph) = (self.photo.0 as f32, self.photo.1 as f32);
            let (cw, ch) = (self.canvas.0 as f32, self.canvas.1 as f32);
            let hips = crate::pose_track::body::hips(body);
            let world_hips = (body.world[23] + body.world[24]).truncate() * 0.5;
            // Pixels a metre, across the body: the least-squares scale
            // from its metres (round the hips) to its pixels.
            let (mut num, mut den) = (0.0, 0.0);
            for i in 0..body.image.len() {
                if body.visibility[i] >= crate::pose_track::body::VISIBLE {
                    let m = body.world[i].truncate() - world_hips;
                    num += (body.image[i].truncate() - hips).dot(m);
                    den += m.length_squared();
                }
            }
            let per_metre = num / den;
            if !(per_metre.is_finite() && per_metre > 0.0) {
                return None;
            }
            let fit = (cw / pw).min(ch / ph);
            let at = (hips - Vec2::new(pw, ph) * 0.5) * fit + Vec2::new(cw, ch) * 0.5;
            let focal = ch * 0.5 / (self.camera.fov.clamp(1.0, 170.0).to_radians() * 0.5).tan();
            let depth = focal / (per_metre * fit);
            let aspect = cw / ch.max(1.0);
            let view_proj = self.camera.projection(aspect) * self.camera.view();
            let (origin, dir) = crate::canvas::figure::unproject(
                &view_proj,
                at.to_array(),
                self.canvas.0,
                self.canvas.1,
            );
            let eye = self.camera.eye();
            let forward = (glam::Vec3::from(self.camera.target) - eye).try_normalize()?;
            let along = dir.dot(forward);
            (along > 1e-3).then(|| origin + dir * ((depth - (origin - eye).dot(forward)) / along))
        }
    }

    /// `figure` (of `model`) posed as `body` in a photo: its hips turned as
    /// the body is to the camera (if `turn`), and moved where it stands (if
    /// `place`). Whether the body made a pose.
    fn pose_from_photo(
        model: &crate::canvas::figure::FigureModel,
        figure: &mut crate::canvas::figure::FigureInstance,
        body: &BodyLandmarks,
        view: &PhotoView,
        hands: bool,
        turn: bool,
        place: bool,
    ) -> bool {
        use crate::canvas::figure::retarget;
        use crate::canvas::figure::skeleton::HumanJoint;
        use glam::{Quat, Vec3};
        let seen = SeenBody::from_landmarks(body, 0.0);
        let hands = if hands { seen.hands } else { [None, None] };
        let Some(mut posed) = retarget::retarget_body(
            &model.skeleton,
            &seen.body,
            [hands[0].as_ref(), hands[1].as_ref()],
        ) else {
            return false;
        };
        // The hips' turn is read as from facing the camera head on: to the
        // layer's camera, from the figure's own turn.
        if turn && let Some(hips) = posed.pose.get(HumanJoint::Hips) {
            let camera = Quat::from_mat4(&view.camera.view().inverse()).normalize();
            let root = Quat::from_array(figure.rotation).normalize();
            posed
                .pose
                .set(HumanJoint::Hips, root.inverse() * camera * hips);
        }
        let which = move |h: HumanJoint| turn || h != HumanJoint::Hips;
        posed
            .pose
            .apply(&model.skeleton, &mut figure.pose, which, false);
        if place
            && let Some(target) = view.hips(body)
            && let Some(i) = model.skeleton.find(HumanJoint::Hips)
        {
            let now = figure.world(model)[i].w_axis.truncate();
            figure.position = (Vec3::from(figure.position) + target - now).to_array();
        }
        true
    }

    /// A picture as the models read it: RGB, its longest side at most
    /// [`PHOTO_SIDE`].
    fn photo_rgb(img: image::RgbaImage) -> image::RgbImage {
        let longest = img.width().max(img.height());
        let img = if longest > PHOTO_SIDE {
            let k = PHOTO_SIDE as f32 / longest as f32;
            let w = ((img.width() as f32 * k).round() as u32).max(1);
            let h = ((img.height() as f32 * k).round() as u32).max(1);
            crate::app::import::downscale(&img, w, h)
        } else {
            img
        };
        image::DynamicImage::ImageRgba8(img).to_rgb8()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use crate::canvas::figure::presets::{HAND_PRESETS, hand_pose};
    use crate::canvas::figure::skeleton::{Finger, TurnAngle};
    use eframe::egui::Color32;
    use glam::{Mat4, Quat};

    fn app_with_figure() -> PainterApp {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.add_figure_layer().unwrap();
        app
    }

    /// A left hand's landmarks, the mannequin's hand posed `shape`, as a
    /// camera in front of it sees them.
    fn seen(shape: &str, time: f64) -> SeenHand {
        let model = crate::canvas::figure::mannequin::mannequin();
        let s = &model.skeleton;
        let shape = HAND_PRESETS.iter().find(|p| p.name == shape).unwrap().shape;
        let pose = hand_pose(s, Side::Left, &shape, |_| true);
        let mut turns = Vec::new();
        pose.apply(s, &mut turns, is_finger, false);
        let turns: Vec<Quat> = turns.iter().map(|q| Quat::from_array(*q)).collect();
        let world = s.world(Mat4::IDENTITY, &turns);
        let at = |h| world[s.find(h).unwrap()].w_axis.truncate();
        let mut points = [Vec3::ZERO; HAND_LANDMARKS];
        points[0] = at(HumanJoint::Hand(Side::Left));
        for (f, finger) in Finger::ALL.into_iter().enumerate() {
            let base = [1, 5, 9, 13, 17][f];
            for k in 0..3u8 {
                points[base + k as usize] = at(HumanJoint::Finger(Side::Left, finger, k));
            }
            let last = s.find(HumanJoint::Finger(Side::Left, finger, 2)).unwrap();
            points[base + 3] = world[last].transform_point3(s.bone_axis(last) * 0.02);
        }
        // The camera's axes: y down, z away.
        let camera = Quat::from_rotation_x(std::f32::consts::PI);
        SeenHand {
            points: points.map(|p| camera * p),
            side: Side::Left,
            time,
        }
    }

    fn turn(app: &PainterApp, h: HumanJoint) -> Quat {
        let s = &crate::canvas::figure::mannequin::mannequin().skeleton;
        app.active_figure().unwrap().scene[0].joint_pose(s.find(h).unwrap())
    }

    fn steps(app: &PainterApp) -> usize {
        app.layer_state.history.labels().0.len()
    }

    #[test]
    fn a_live_stream_of_hands_is_one_undo_step() {
        let mut app = app_with_figure();
        let before = steps(&app);
        let curl = HumanJoint::Finger(Side::Left, Finger::Index, 1);
        app.workspace.figure.scanner.target = TargetHand::Left;
        app.scanner_set_live(true);
        let stream = [
            "Open", "Relaxed", "Fist", "Fist", "Fist", "Fist", "Fist", "Fist",
        ];
        for (frame, shape) in stream.into_iter().enumerate() {
            assert!(app.scanner_apply(&seen(shape, frame as f64 / 15.0), true));
        }
        app.scanner_set_live(false);
        assert_eq!(steps(&app), before + 1, "one step for the whole stream");
        assert!(
            app.layer_state.history.labels().0.last().unwrap() == "Hand Scanner",
            "{:?}",
            app.layer_state.history.labels().0
        );
        // Smoothed, but it got most of the way to the fist.
        assert!(turn(&app, curl).turn_angle(Quat::IDENTITY).to_degrees() > 50.0);
        app.apply_history(false);
        assert!(turn(&app, curl).turn_angle(Quat::IDENTITY) < 1e-4);
    }

    #[test]
    fn a_capture_is_a_step_of_its_own_and_poses_the_chosen_hand() {
        let mut app = app_with_figure();
        let before = steps(&app);
        // A left hand seen, the right one posed: as in a mirror.
        app.workspace.figure.scanner.target = TargetHand::Right;
        assert!(app.scanner_apply(&seen("Fist", 0.0), false));
        assert!(app.scanner_apply(&seen("Point", 0.0), false));
        assert_eq!(steps(&app), before + 2);
        let index = HumanJoint::Finger(Side::Right, Finger::Index, 1);
        let middle = HumanJoint::Finger(Side::Right, Finger::Middle, 1);
        assert!(turn(&app, index).turn_angle(Quat::IDENTITY).to_degrees() < 3.0);
        assert!(turn(&app, middle).turn_angle(Quat::IDENTITY).to_degrees() > 90.0);
        // The left hand wasn't touched.
        let left = HumanJoint::Finger(Side::Left, Finger::Middle, 1);
        assert!(turn(&app, left).turn_angle(Quat::IDENTITY) < 1e-6);
        // Auto takes the hand seen.
        app.workspace.figure.scanner.target = TargetHand::Auto;
        assert!(app.scanner_apply(&seen("Fist", 0.0), false));
        assert!(turn(&app, left).turn_angle(Quat::IDENTITY).to_degrees() > 90.0);
    }

    #[test]
    fn nothing_happens_without_a_figure_layer() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        assert!(!app.scanner_apply(&seen("Fist", 0.0), false));
    }

    #[cfg(feature = "pose-capture")]
    #[test]
    fn the_camera_permission_is_asked_for_once_while_the_window_is_open() {
        use crate::capture::tests::TestCamera;
        use std::sync::atomic::Ordering;
        let mut app = app_with_figure();
        app.open_hand_scanner();
        let refused = || {
            let mut camera = TestCamera::new(vec![image::RgbImage::new(8, 8)]);
            camera.permission = false;
            camera
        };
        let no_models = || Some(crate::pose_track::PoseWorker::without_models());
        let camera = refused();
        let asked = camera.asked.clone();
        app.scanner_begin(Box::new(camera), no_models());
        let session = app.workspace.figure.scanner.session.as_ref().unwrap();
        assert!(session.needs_permission() && !session.running());
        assert_eq!(asked.load(Ordering::Relaxed), 1);
        // The prompt took the focus: the camera stops; back, it isn't
        // asked for again.
        app.scanner_poll(false);
        assert!(app.workspace.figure.scanner.session.is_none());
        let camera = refused();
        let again = camera.asked.clone();
        app.scanner_begin(Box::new(camera), no_models());
        assert_eq!(again.load(Ordering::Relaxed), 0);
        // Allowed later (in the system's settings): it starts.
        let mut allowed = refused();
        allowed.permission = true;
        app.scanner_begin(Box::new(allowed), no_models());
        assert!(
            app.workspace
                .figure
                .scanner
                .session
                .as_ref()
                .unwrap()
                .running()
        );
        // Closed and opened again, it asks again.
        app.close_hand_scanner();
        app.open_hand_scanner();
        let camera = refused();
        let reopened = camera.asked.clone();
        app.scanner_begin(Box::new(camera), no_models());
        assert_eq!(reopened.load(Ordering::Relaxed), 1);
    }

    #[cfg(feature = "pose-capture")]
    #[test]
    fn the_scanner_poses_the_figure_from_a_camera_live_as_one_step() {
        use crate::capture::tests::TestCamera;
        let Some(worker) = session::test_worker() else {
            return;
        };
        let photo = image::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/hand-photo.jpg"),
        )
        .unwrap()
        .to_rgb8();
        let mut app = app_with_figure();
        let before = steps(&app);
        app.open_hand_scanner();
        app.scanner_begin(Box::new(TestCamera::new(vec![photo])), Some(worker));
        app.scanner_set_live(true);
        let mut posed = 0;
        let end = Instant::now() + std::time::Duration::from_secs(60);
        while posed < 3 && Instant::now() < end {
            app.scanner_poll(true);
            let session = app.workspace.figure.scanner.session.as_ref().unwrap();
            assert!(session.error.is_none(), "{:?}", session.error);
            if let Some(t) = &session.tracked
                && t.hand.is_some()
            {
                posed += 1;
                app.workspace
                    .figure
                    .scanner
                    .session
                    .as_mut()
                    .unwrap()
                    .tracked = None;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(posed >= 3, "the photo's hand was tracked");
        // The hand in the photo is a left one, held open.
        let curl = turn(&app, HumanJoint::Finger(Side::Left, Finger::Middle, 1));
        assert!(curl.turn_angle(Quat::IDENTITY).to_degrees() < 35.0);
        // Losing focus stops the camera and ends the step.
        app.scanner_poll(false);
        assert!(app.workspace.figure.scanner.session.is_none());
        assert!(!app.workspace.figure.scanner.live);
        assert_eq!(steps(&app), before + 1);
        app.close_hand_scanner();
    }

    /// The mannequin's landmarks posed `preset`, as a camera in front of
    /// it sees them.
    fn seen_body(preset: &str, time: f64) -> SeenBody {
        use crate::canvas::figure::retarget::tests::{body_landmarks, body_preset};
        let model = crate::canvas::figure::mannequin::mannequin();
        SeenBody {
            body: body_landmarks(&model.skeleton, &body_preset(preset), Quat::IDENTITY),
            hands: [None, None],
            time,
        }
    }

    fn arm_raise(app: &PainterApp, side: Side) -> f32 {
        turn(app, HumanJoint::UpperArm(side))
            .turn_angle(Quat::IDENTITY)
            .to_degrees()
    }

    #[test]
    fn a_live_stream_of_bodies_is_one_undo_step() {
        let mut app = app_with_figure();
        // From T-pose.
        app.figure_step("Reset", |layer| layer.scene[0].pose.clear());
        let before = steps(&app);
        app.workspace.figure.scanner.mode = ScanMode::Body;
        app.scanner_set_live(true);
        let stream = [
            "T-pose", "A-pose", "Standing", "Standing", "Standing", "Standing",
        ];
        for (frame, preset) in stream.into_iter().enumerate() {
            let seen = seen_body(preset, frame as f64 / 15.0);
            assert!(app.body_apply(&seen, true, false, "Pose from Camera"));
        }
        app.scanner_set_live(false);
        assert_eq!(steps(&app), before + 1, "one step for the whole stream");
        assert_eq!(
            app.layer_state.history.labels().0.last().unwrap(),
            "Pose from Camera"
        );
        // Smoothed, but most of the way down to standing (74°).
        assert!(
            arm_raise(&app, Side::Left) > 55.0,
            "{}",
            arm_raise(&app, Side::Left)
        );
        app.apply_history(false);
        assert!(arm_raise(&app, Side::Left) < 1e-3);
    }

    #[test]
    fn a_body_capture_is_one_step_and_mirror_swaps_sides() {
        let mut app = app_with_figure();
        app.figure_step("Reset", |layer| layer.scene[0].pose.clear());
        let before = steps(&app);
        // The right arm reaching forward.
        let seen = seen_body("Reaching", 0.0);
        assert!(app.body_apply(&seen, false, false, "Pose from Photo"));
        assert_eq!(steps(&app), before + 1);
        let reach = |app: &PainterApp, side| {
            let s = &crate::canvas::figure::mannequin::mannequin().skeleton;
            let layer = app.active_figure().unwrap();
            let turns: Vec<Quat> = (0..s.len()).map(|i| layer.scene[0].joint_pose(i)).collect();
            let world = s.world(glam::Mat4::IDENTITY, &turns);
            let hand = world[s.find(HumanJoint::Hand(side)).unwrap()].w_axis;
            hand.z
        };
        assert!(
            reach(&app, Side::Right) > 0.4,
            "{}",
            reach(&app, Side::Right)
        );
        assert!(reach(&app, Side::Left) < 0.2);
        // As a mirror image: the left arm reaches.
        assert!(app.body_apply(&seen, false, true, "Pose from Photo"));
        assert_eq!(steps(&app), before + 2);
        assert!(reach(&app, Side::Left) > 0.4 && reach(&app, Side::Right) < 0.2);
        // Without a figure layer, nothing.
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut empty = crate::project::tests::test_app_pub(canvas);
        assert!(!empty.body_apply(&seen, false, false, "Pose from Photo"));
    }

    #[test]
    fn turning_the_figure_can_be_left_out() {
        let mut app = app_with_figure();
        let model = crate::canvas::figure::mannequin::mannequin();
        let turned = SeenBody {
            body: crate::canvas::figure::retarget::tests::body_landmarks(
                &model.skeleton,
                &crate::canvas::figure::retarget::tests::body_preset("Standing"),
                Quat::from_rotation_y(0.8),
            ),
            hands: [None, None],
            time: 0.0,
        };
        app.workspace.figure.scanner.turn_body = false;
        assert!(app.body_apply(&turned, false, false, "Pose from Photo"));
        assert!(turn(&app, HumanJoint::Hips).turn_angle(Quat::IDENTITY) < 1e-4);
        app.workspace.figure.scanner.turn_body = true;
        assert!(app.body_apply(&turned, false, false, "Pose from Photo"));
        let hips = turn(&app, HumanJoint::Hips).turn_angle(Quat::IDENTITY);
        assert!((hips - 0.8).abs() < 0.05, "{hips}");
    }

    #[cfg(feature = "pose-capture")]
    fn body_photo() -> std::sync::Arc<image::RgbImage> {
        std::sync::Arc::new(
            image::open(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/body-photo.jpg"),
            )
            .unwrap()
            .to_rgb8(),
        )
    }

    /// How far the elbow bends.
    fn elbow(app: &PainterApp, side: Side) -> f32 {
        turn(app, HumanJoint::LowerArm(side))
            .turn_angle(Quat::IDENTITY)
            .to_degrees()
    }

    /// Two people picked in a photo: two figures, in one undo step, each
    /// standing where its body is (its hips drawn on the canvas where the
    /// body's are in the photo fitted to it), the smaller one farther.
    #[cfg(feature = "pose-capture")]
    #[test]
    fn people_picked_in_a_photo_stand_as_they_do_in_it() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pose-models/v1");
        if !crate::pose_track::is_installed(&dir) {
            return;
        }
        let image = body_photo();
        let (pw, ph) = image.dimensions();
        let body = crate::pose_track::read_photo(&image, false)
            .unwrap()
            .remove(0);
        // A second person: the same body, half the size, to the right.
        let mut far = body.clone();
        let hips = crate::pose_track::body::hips(&body);
        let to = glam::Vec2::new(pw as f32 * 0.8, ph as f32 * 0.5);
        for p in &mut far.image {
            let xy = (p.truncate() - hips) * 0.5 + to;
            *p = xy.extend(p.z * 0.5);
        }
        let mut app = app_with_figure();
        let before = steps(&app);
        app.workspace.figure.scanner.photo = Some(session::PhotoPose {
            name: "two".into(),
            image: Some(image.clone()),
            bodies: vec![body.clone(), far.clone()],
            chosen: vec![true, true],
            reading: false,
            error: None,
            texture: None,
        });
        assert!(app.photo_apply());
        assert_eq!(steps(&app), before + 1);
        let layer = app.active_figure().unwrap();
        assert_eq!(layer.scene.len(), 2);
        let (cw, ch) = (app.canvas.width(), app.canvas.height());
        let view_proj = layer.view_proj(cw, ch);
        let fit = (cw as f32 / pw as f32).min(ch as f32 / ph as f32);
        let mut depths = Vec::new();
        for (figure, seen) in layer.scene.iter().zip([&body, &far]) {
            let model = figure.model.resolve().unwrap();
            let i = model.skeleton.find(HumanJoint::Hips).unwrap();
            let at = figure.world(&model)[i].w_axis.truncate();
            let (px, _) = crate::canvas::figure::project(&view_proj, at, cw, ch).unwrap();
            let want = (crate::pose_track::body::hips(seen)
                - glam::Vec2::new(pw as f32, ph as f32) * 0.5)
                * fit
                + glam::Vec2::new(cw as f32, ch as f32) * 0.5;
            assert!(
                glam::Vec2::from(px).distance(want) < 0.5,
                "{px:?} vs {want}"
            );
            depths.push(at.distance(layer.camera.eye()));
        }
        let ratio = depths[1] / depths[0];
        assert!(
            (ratio - 2.0).abs() < 0.1,
            "half the size, twice as far: {ratio}"
        );
    }

    #[cfg(feature = "pose-capture")]
    #[test]
    fn a_photo_is_applied_as_one_step() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pose-models/v1");
        if !crate::pose_track::is_installed(&dir) {
            return;
        }
        let mut app = app_with_figure();
        let before = steps(&app);
        // Nothing read yet: nothing to apply.
        app.photo_read("x", Err("ignored: no photo window".into()));
        assert!(!app.photo_apply());
        let image = body_photo();
        let t = Instant::now();
        let body = crate::pose_track::read_photo(&image, true).unwrap();
        eprintln!("photo read in {:?}", t.elapsed());
        assert!(!body.is_empty(), "the man in the doorway is found");
        app.workspace.figure.scanner.photo = Some(session::PhotoPose {
            name: "body-photo.jpg".into(),
            image: None,
            bodies: Vec::new(),
            chosen: Vec::new(),
            reading: true,
            error: None,
            texture: None,
        });
        app.photo_read("body-photo.jpg", Ok((image, body)));
        assert!(app.photo_apply());
        assert_eq!(steps(&app), before + 1);
        assert_eq!(
            app.layer_state.history.labels().0.last().unwrap(),
            "Pose from Photo"
        );
        assert!(
            app.workspace.figure.scanner.photo.is_none(),
            "the window closes"
        );
        // Hands on hips: both elbows bent well past standing's 15°, the
        // arms down from T-pose.
        for side in [Side::Left, Side::Right] {
            assert!(
                elbow(&app, side) > 50.0,
                "{side:?} elbow {}",
                elbow(&app, side)
            );
            assert!(
                arm_raise(&app, side) > 30.0,
                "{side:?} arm {}",
                arm_raise(&app, side)
            );
        }
    }

    #[cfg(feature = "pose-capture")]
    #[test]
    fn pose_from_camera_poses_the_body_live_as_one_step() {
        use crate::capture::tests::TestCamera;
        let Some(worker) = session::test_worker() else {
            return;
        };
        let mut app = app_with_figure();
        let before = steps(&app);
        app.open_pose_from_camera();
        assert_eq!(app.workspace.figure.scanner.mode, ScanMode::Body);
        let photo = (*body_photo()).clone();
        app.scanner_begin(Box::new(TestCamera::new(vec![photo])), Some(worker));
        app.scanner_set_live(true);
        let mut posed = 0;
        let end = Instant::now() + std::time::Duration::from_secs(60);
        while posed < 3 && Instant::now() < end {
            app.scanner_poll(true);
            let session = app.workspace.figure.scanner.session.as_mut().unwrap();
            assert!(session.error.is_none(), "{:?}", session.error);
            if let Some(t) = session.tracked.take() {
                assert!(t.hand.is_none());
                if t.body.is_some() {
                    eprintln!("body {} ms", t.millis);
                    posed += 1;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(posed >= 3, "the photo's body was tracked");
        for side in [Side::Left, Side::Right] {
            assert!(
                elbow(&app, side) > 40.0,
                "{side:?} elbow {}",
                elbow(&app, side)
            );
        }
        // Switching to hands ends the step; so does losing focus.
        app.open_hand_scanner();
        assert!(!app.workspace.figure.scanner.live);
        app.scanner_poll(false);
        assert!(app.workspace.figure.scanner.session.is_none());
        assert_eq!(steps(&app), before + 1);
        app.close_hand_scanner();
    }
}
