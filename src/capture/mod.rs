//! The camera, for the Hand Scanner and Pose from Camera (feature
//! `pose-capture`): a small [`Camera`] interface the camera window drives, with a webcam behind it on
//! Linux and Windows (`desktop`, through nokhwa), the device's cameras on
//! Android (`android`, Camera2 through `CameraFeed.java`), and none
//! elsewhere yet. Frames stay on the device: they go to the hand or body
//! tracker and the preview, and nowhere else.

#[cfg(target_os = "android")]
pub mod android;
#[cfg(any(target_os = "linux", windows))]
pub mod desktop;

pub use crate::pose_track::Frame;

/// A camera the system has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    /// Shown in the app, and what the choice is remembered by.
    pub name: String,
}

/// Why there are no frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CameraError {
    /// The system has no camera (or none the app can use).
    NoCamera,
    /// The app isn't allowed to use the camera (yet): ask.
    #[cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]
    PermissionNeeded,
    /// This system's cameras aren't supported.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    Unsupported,
    Failed(String),
}

impl std::fmt::Display for CameraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCamera => f.write_str("No camera found"),
            Self::PermissionNeeded => f.write_str("The app needs permission to use the camera"),
            Self::Unsupported => f.write_str("The camera isn't supported on this system yet"),
            Self::Failed(why) => write!(f, "The camera failed: {why}"),
        }
    }
}

/// A camera the camera window can run: list the cameras, start one, take the
/// newest frame now and then, stop it.
pub trait Camera: Send {
    /// The cameras there are.
    fn list(&mut self) -> Result<Vec<CameraInfo>, CameraError>;
    /// Start camera `index` (of [`Camera::list`]), at about `size` (width,
    /// height); a running one stops first. Frames may take a moment to
    /// come, and a failure while starting shows in [`Camera::error`].
    fn start(&mut self, index: usize, size: [u32; 2]) -> Result<(), CameraError>;
    /// The newest frame, if there's one newer than the last taken.
    fn latest_frame(&mut self) -> Option<Frame>;
    /// Why the camera stopped, if it stopped by itself.
    fn error(&mut self) -> Option<CameraError>;
    fn stop(&mut self);
    /// Whether the app may use the camera (Android asks the user).
    fn has_permission(&self) -> bool {
        true
    }
    /// Ask the user to let the app use the camera; the answer shows in
    /// [`Camera::has_permission`] later.
    fn request_permission(&mut self) {}
}

/// This system's camera.
pub fn system_camera() -> Box<dyn Camera> {
    #[cfg(any(target_os = "linux", windows))]
    return Box::new(desktop::DesktopCamera::default());
    #[cfg(target_os = "android")]
    return Box::new(android::AndroidCamera::default());
    #[cfg(not(any(target_os = "linux", windows, target_os = "android")))]
    return Box::new(NoCamera);
}

/// No camera support (macOS and iOS for now).
#[cfg_attr(
    any(target_os = "linux", windows, target_os = "android"),
    allow(dead_code)
)]
pub struct NoCamera;

impl Camera for NoCamera {
    fn list(&mut self) -> Result<Vec<CameraInfo>, CameraError> {
        Err(CameraError::Unsupported)
    }
    fn start(&mut self, _: usize, _: [u32; 2]) -> Result<(), CameraError> {
        Err(CameraError::Unsupported)
    }
    fn latest_frame(&mut self) -> Option<Frame> {
        None
    }
    fn error(&mut self) -> Option<CameraError> {
        None
    }
    fn stop(&mut self) {}
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::Arc;

    /// A camera that plays back pictures, for tests: each start begins
    /// again from the first.
    pub struct TestCamera {
        pub pictures: Vec<Arc<image::RgbImage>>,
        pub running: bool,
        pub permission: bool,
        /// How many times the user was asked for the camera.
        pub asked: Arc<std::sync::atomic::AtomicUsize>,
        next: usize,
        seq: u64,
    }

    impl TestCamera {
        pub fn new(pictures: Vec<image::RgbImage>) -> Self {
            Self {
                pictures: pictures.into_iter().map(Arc::new).collect(),
                running: false,
                permission: true,
                asked: Arc::default(),
                next: 0,
                seq: 0,
            }
        }
    }

    impl Camera for TestCamera {
        fn list(&mut self) -> Result<Vec<CameraInfo>, CameraError> {
            if self.pictures.is_empty() {
                return Err(CameraError::NoCamera);
            }
            Ok(vec![CameraInfo {
                name: "Test camera".into(),
            }])
        }
        fn start(&mut self, index: usize, _: [u32; 2]) -> Result<(), CameraError> {
            if !self.permission {
                return Err(CameraError::PermissionNeeded);
            }
            if index >= self.list()?.len() {
                return Err(CameraError::NoCamera);
            }
            self.running = true;
            self.next = 0;
            Ok(())
        }
        fn latest_frame(&mut self) -> Option<Frame> {
            if !self.running {
                return None;
            }
            let image = self.pictures[self.next % self.pictures.len()].clone();
            self.next += 1;
            self.seq += 1;
            Some(Frame {
                seq: self.seq,
                image,
            })
        }
        fn error(&mut self) -> Option<CameraError> {
            None
        }
        fn stop(&mut self) {
            self.running = false;
        }
        fn has_permission(&self) -> bool {
            self.permission
        }
        fn request_permission(&mut self) {
            self.asked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn a_camera_plays_frames_only_while_started() {
        let mut camera: Box<dyn Camera> =
            Box::new(TestCamera::new(vec![image::RgbImage::new(4, 4)]));
        assert_eq!(camera.list().unwrap().len(), 1);
        assert!(camera.latest_frame().is_none());
        camera.start(0, [320, 240]).unwrap();
        let a = camera.latest_frame().unwrap();
        let b = camera.latest_frame().unwrap();
        assert!(b.seq > a.seq);
        camera.stop();
        assert!(camera.latest_frame().is_none());
        assert_eq!(camera.start(3, [320, 240]), Err(CameraError::NoCamera));
        let mut none: Box<dyn Camera> = Box::new(TestCamera::new(Vec::new()));
        assert_eq!(none.list(), Err(CameraError::NoCamera));
    }
}
