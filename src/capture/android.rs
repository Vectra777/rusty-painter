//! The device's cameras on Android: Camera2 in `CameraFeed.java`, which
//! leaves each frame (RGB, upright, about 320×240, 15 a second) in one
//! direct buffer mapped here once. Each poll asks Java for a frame newer
//! than the last (one call; nothing is copied unless there is one), holds
//! the buffer while copying it out, and lets it go.

use std::sync::Arc;

use super::{Camera, CameraError, CameraInfo, Frame};

/// The frame size asked for.
const SIZE: [i32; 2] = [320, 240];

#[derive(Default)]
pub struct AndroidCamera {
    /// The cameras last listed: their ids.
    ids: Vec<String>,
    running: Option<Running>,
}

struct Running {
    /// Keeps the buffer alive while it's mapped.
    _buffer: jni::objects::GlobalRef,
    address: *mut u8,
    capacity: usize,
    last: i64,
}

// The mapped buffer is only read on the thread that holds the camera (the
// UI's), between `begin` and `end`.
unsafe impl Send for Running {}

impl Camera for AndroidCamera {
    fn list(&mut self) -> Result<Vec<CameraInfo>, CameraError> {
        let cameras = crate::android::camera_list().map_err(CameraError::Failed)?;
        if cameras.is_empty() {
            return Err(CameraError::NoCamera);
        }
        self.ids = cameras.iter().map(|(id, _)| id.clone()).collect();
        Ok(cameras
            .into_iter()
            .map(|(_, name)| CameraInfo { name })
            .collect())
    }

    fn start(&mut self, index: usize, _size: [u32; 2]) -> Result<(), CameraError> {
        self.stop();
        if !self.has_permission() {
            return Err(CameraError::PermissionNeeded);
        }
        if self.ids.is_empty() {
            self.list()?;
        }
        let id = self.ids.get(index).ok_or(CameraError::NoCamera)?;
        let (buffer, address, capacity) =
            crate::android::camera_start(id, SIZE[0], SIZE[1]).map_err(CameraError::Failed)?;
        if address.is_null() || capacity < 8 {
            crate::android::camera_stop();
            return Err(CameraError::Failed("no frame buffer".into()));
        }
        self.running = Some(Running {
            _buffer: buffer,
            address,
            capacity,
            last: 0,
        });
        Ok(())
    }

    fn latest_frame(&mut self) -> Option<Frame> {
        let running = self.running.as_mut()?;
        let seq = crate::android::camera_begin(running.last).ok()?;
        if seq < 0 {
            return None;
        }
        // The buffer is ours until `camera_end`: Java writes it only
        // between frames we aren't reading.
        let bytes = unsafe { std::slice::from_raw_parts(running.address, running.capacity) };
        let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let len = width as usize * height as usize * 3;
        let image = (8 + len <= bytes.len())
            .then(|| image::RgbImage::from_raw(width, height, bytes[8..8 + len].to_vec()))
            .flatten();
        let _ = crate::android::camera_end();
        running.last = seq;
        Some(Frame {
            seq: seq as u64,
            image: Arc::new(image?),
        })
    }

    fn error(&mut self) -> Option<CameraError> {
        self.running.as_ref()?;
        crate::android::camera_error().map(CameraError::Failed)
    }

    fn stop(&mut self) {
        if self.running.take().is_some() {
            crate::android::camera_stop();
        }
    }

    fn has_permission(&self) -> bool {
        crate::android::has_camera_access()
    }

    fn request_permission(&mut self) {
        if let Err(err) = crate::android::request_camera_access() {
            log::warn!("Couldn't ask for the camera: {err}");
        }
    }
}

impl Drop for AndroidCamera {
    fn drop(&mut self) {
        self.stop();
    }
}
