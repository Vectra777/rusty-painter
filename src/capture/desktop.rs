//! Webcams on Linux (V4L2) and Windows (Media Foundation), through nokhwa:
//! a thread reads frames, converts them to RGB and leaves the newest for
//! the camera window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
};

use super::{Camera, CameraError, CameraInfo, Frame};

/// The formats the app converts itself (nokhwa's own MJPEG decoder is C).
const FORMATS: [FrameFormat; 6] = [
    FrameFormat::MJPEG,
    FrameFormat::YUYV,
    FrameFormat::NV12,
    FrameFormat::RAWRGB,
    FrameFormat::RAWBGR,
    FrameFormat::GRAY,
];

/// Frames wider than this are shrunk before they're passed on.
const MAX_WIDTH: u32 = 640;

#[derive(Default)]
pub struct DesktopCamera {
    /// The cameras last listed, by nokhwa's index.
    found: Vec<(CameraInfo, CameraIndex)>,
    running: Option<Running>,
    /// The last capture thread, told to stop: it lets the camera go after
    /// the frame it's waiting for (not waited for, so a camera that stops
    /// sending can't freeze the app).
    stopping: Option<std::thread::JoinHandle<()>>,
}

struct Running {
    stop: Arc<AtomicBool>,
    slot: FrameSlot,
    thread: std::thread::JoinHandle<()>,
}

impl Drop for DesktopCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Camera for DesktopCamera {
    fn list(&mut self) -> Result<Vec<CameraInfo>, CameraError> {
        let found =
            nokhwa::query(ApiBackend::Auto).map_err(|e| CameraError::Failed(e.to_string()))?;
        // Only devices that give pictures the app reads (Linux lists a
        // camera's metadata nodes too), colour ones first (an infrared
        // camera beside the webcam gives grey).
        let mut usable: Vec<(bool, String, CameraIndex)> = found
            .into_iter()
            .filter_map(|info| {
                let any = RequestedFormat::with_formats(RequestedFormatType::None, &FORMATS);
                let mut camera = nokhwa::Camera::new(info.index().clone(), any).ok()?;
                let formats = camera.compatible_camera_formats().ok()?;
                let grey = formats.iter().all(|f| f.format() == FrameFormat::GRAY);
                (!formats.is_empty()).then(|| (grey, info.human_name(), info.index().clone()))
            })
            .collect();
        usable.sort_by_key(|(grey, ..)| *grey);
        self.found.clear();
        for (_, name, index) in usable {
            // Two cameras of one name (the same model twice) are numbered.
            let same = (self.found.iter())
                .filter(|(c, _)| c.name.starts_with(&name))
                .count();
            let name = if same == 0 {
                name
            } else {
                format!("{name} ({})", same + 1)
            };
            self.found.push((CameraInfo { name }, index));
        }
        if self.found.is_empty() {
            return Err(CameraError::NoCamera);
        }
        Ok(self.found.iter().map(|(info, _)| info.clone()).collect())
    }

    fn start(&mut self, index: usize, size: [u32; 2]) -> Result<(), CameraError> {
        self.stop();
        // The camera is busy until the last thread lets it go.
        if let Some(last) = self.stopping.take() {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(1500);
            while !last.is_finished() && std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        if self.found.is_empty() {
            self.list()?;
        }
        let camera = self
            .found
            .get(index)
            .map(|(_, i)| i.clone())
            .ok_or(CameraError::NoCamera)?;
        let stop = Arc::new(AtomicBool::new(false));
        let slot = FrameSlot::default();
        let thread = {
            let (stop, slot) = (stop.clone(), slot.clone());
            std::thread::Builder::new()
                .name("camera".into())
                .spawn(move || {
                    if let Err(err) = capture(camera, size, &stop, &slot) {
                        slot.fail(err);
                    }
                })
                .map_err(|e| CameraError::Failed(e.to_string()))?
        };
        self.running = Some(Running { stop, slot, thread });
        Ok(())
    }

    fn latest_frame(&mut self) -> Option<Frame> {
        self.running.as_ref()?.slot.take()
    }

    fn error(&mut self) -> Option<CameraError> {
        self.running.as_ref()?.slot.error()
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop.store(true, Ordering::Relaxed);
            self.stopping = Some(running.thread);
        }
    }
}

/// The format nearest `size` among those the app can read, at the frame
/// rate nearest 30.
fn pick(formats: &[CameraFormat], size: [u32; 2]) -> Option<CameraFormat> {
    let want = (size[0] * size[1]) as i64;
    formats
        .iter()
        .filter(|f| FORMATS.contains(&f.format()))
        .min_by_key(|f| {
            let area = (f.width() * f.height()) as i64;
            // Too small reads worse than too big.
            let off = if area < want {
                (want - area) * 4
            } else {
                area - want
            };
            // A raw format beats MJPEG at the same size (no decoding).
            let mjpeg = (f.format() == FrameFormat::MJPEG) as i64;
            (off, (f.frame_rate() as i64 - 30).abs(), mjpeg)
        })
        .copied()
}

fn failed(e: impl std::fmt::Display) -> CameraError {
    CameraError::Failed(e.to_string())
}

fn capture(
    index: CameraIndex,
    size: [u32; 2],
    stop: &AtomicBool,
    slot: &FrameSlot,
) -> Result<(), CameraError> {
    let any = RequestedFormat::with_formats(RequestedFormatType::None, &FORMATS);
    let mut camera = nokhwa::Camera::new(index, any).map_err(failed)?;
    let formats = camera.compatible_camera_formats().map_err(failed)?;
    let format = pick(&formats, size).ok_or(CameraError::Unsupported)?;
    camera
        .set_camera_requset(RequestedFormat::with_formats(
            RequestedFormatType::Exact(format),
            &FORMATS,
        ))
        .map_err(failed)?;
    camera.open_stream().map_err(failed)?;
    let mut seq = 0;
    while !stop.load(Ordering::Relaxed) {
        let buffer = camera.frame().map_err(failed)?;
        let res = buffer.resolution();
        let (w, h) = (res.width(), res.height());
        let data = buffer.buffer();
        let image = match buffer.source_frame_format() {
            FrameFormat::MJPEG => {
                image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
                    .ok()
                    .map(|i| i.to_rgb8())
            }
            FrameFormat::YUYV => yuyv_to_rgb(data, w, h),
            FrameFormat::NV12 => nv12_to_rgb(data, w, h),
            FrameFormat::RAWRGB => image::RgbImage::from_raw(w, h, data.to_vec()),
            FrameFormat::RAWBGR => image::RgbImage::from_raw(
                w,
                h,
                (data.as_chunks::<3>().0.iter())
                    .flat_map(|p| [p[2], p[1], p[0]])
                    .collect(),
            ),
            FrameFormat::GRAY => image::GrayImage::from_raw(w, h, data.to_vec())
                .map(|g| image::DynamicImage::ImageLuma8(g).to_rgb8()),
        };
        // A frame that didn't decode (a torn MJPEG one) is skipped.
        let Some(image) = image else {
            continue;
        };
        seq += 1;
        slot.put(Frame {
            seq,
            image: Arc::new(shrink(image, MAX_WIDTH)),
        });
    }
    let _ = camera.stop_stream();
    Ok(())
}

/// Where a capture thread leaves its newest frame (replacing one not
/// taken yet) or why it stopped.
#[derive(Clone, Default)]
pub struct FrameSlot(Arc<Mutex<(Option<Frame>, Option<CameraError>)>>);

impl FrameSlot {
    pub fn put(&self, frame: Frame) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).0 = Some(frame);
    }

    pub fn fail(&self, error: CameraError) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(error);
    }

    pub fn take(&self) -> Option<Frame> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).0.take()
    }

    pub fn error(&self) -> Option<CameraError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).1.clone()
    }
}

/// YUYV (YUV 4:2:2, two pixels in four bytes) to RGB.
pub fn yuyv_to_rgb(data: &[u8], width: u32, height: u32) -> Option<image::RgbImage> {
    let (w, h) = (width as usize, height as usize);
    if w % 2 != 0 || data.len() < w * h * 2 {
        return None;
    }
    let mut out = vec![0u8; w * h * 3];
    let (quads, _) = data[..w * h * 2].as_chunks::<4>();
    let (pixels, _) = out.as_chunks_mut::<6>();
    for (quad, px) in quads.iter().zip(pixels) {
        let (u, v) = (quad[1], quad[3]);
        px[..3].copy_from_slice(&yuv(quad[0], u, v));
        px[3..].copy_from_slice(&yuv(quad[2], u, v));
    }
    image::RgbImage::from_raw(width, height, out)
}

/// NV12 (a Y plane, then interleaved U and V at half size) to RGB.
pub fn nv12_to_rgb(data: &[u8], width: u32, height: u32) -> Option<image::RgbImage> {
    let (w, h) = (width as usize, height as usize);
    let chroma = w.div_ceil(2) * 2 * h.div_ceil(2);
    if data.len() < w * h + chroma {
        return None;
    }
    let (luma, uv) = data.split_at(w * h);
    let row = w.div_ceil(2) * 2;
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let c = (y / 2) * row + (x / 2) * 2;
            let o = (y * w + x) * 3;
            out[o..o + 3].copy_from_slice(&yuv(luma[y * w + x], uv[c], uv[c + 1]));
        }
    }
    image::RgbImage::from_raw(width, height, out)
}

/// One pixel from YUV (BT.601, video range) to RGB.
fn yuv(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = 298 * (y as i32 - 16).max(0);
    let (d, e) = (u as i32 - 128, v as i32 - 128);
    let clamp = |x: i32| ((x + 128) >> 8).clamp(0, 255) as u8;
    [
        clamp(c + 409 * e),
        clamp(c - 100 * d - 208 * e),
        clamp(c + 516 * d),
    ]
}

/// `image` scaled down (by a whole factor, averaging) to at most
/// `max_width` across: a big webcam picture is more than the tracker and
/// the preview need.
pub fn shrink(image: image::RgbImage, max_width: u32) -> image::RgbImage {
    let factor = image.width().div_ceil(max_width.max(1));
    if factor <= 1 {
        return image;
    }
    let (w, h) = (image.width() / factor, image.height() / factor);
    image::RgbImage::from_fn(w, h, |x, y| {
        let mut sum = [0u32; 3];
        for dy in 0..factor {
            for dx in 0..factor {
                let p = image.get_pixel(x * factor + dx, y * factor + dy);
                for k in 0..3 {
                    sum[k] += p[k] as u32;
                }
            }
        }
        let n = factor * factor;
        image::Rgb(sum.map(|s| (s / n) as u8))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nokhwa::utils::Resolution;

    #[test]
    fn the_format_nearest_the_size_asked_for_is_picked() {
        let f = |w, h, format, fps| CameraFormat::new(Resolution::new(w, h), format, fps);
        let formats = [
            f(1920, 1080, FrameFormat::MJPEG, 30),
            f(640, 480, FrameFormat::MJPEG, 30),
            f(640, 480, FrameFormat::YUYV, 30),
            f(320, 240, FrameFormat::YUYV, 30),
            f(640, 480, FrameFormat::YUYV, 5),
        ];
        let got = pick(&formats, [640, 480]).unwrap();
        assert_eq!(got, f(640, 480, FrameFormat::YUYV, 30));
        assert_eq!(pick(&formats[..1], [640, 480]), Some(formats[0]));
        assert_eq!(pick(&[], [640, 480]), None);
    }

    /// The machine's own webcam, if it has one: a frame comes, the
    /// camera stops (nothing is kept). Run by hand: `cargo test --lib
    /// webcam -- --ignored`.
    #[test]
    #[ignore = "opens the webcam"]
    fn the_webcam_gives_frames() {
        let mut camera = DesktopCamera::default();
        let Ok(cameras) = camera.list() else {
            eprintln!("no webcam; skipping");
            return;
        };
        eprintln!("cameras: {cameras:?}");
        camera.start(0, [640, 480]).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut frames = 0;
        while frames < 3 && std::time::Instant::now() < end {
            assert_eq!(camera.error(), None);
            if let Some(frame) = camera.latest_frame() {
                assert!(frame.image.width() > 0 && frame.image.width() <= MAX_WIDTH);
                eprintln!("frame {} {:?}", frame.seq, frame.image.dimensions());
                frames += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        camera.stop();
        assert_eq!(frames, 3);
    }

    #[test]
    fn the_slot_keeps_the_newest_frame_only() {
        let slot = FrameSlot::default();
        for seq in 1..=3 {
            slot.put(Frame {
                seq,
                image: Arc::new(image::RgbImage::new(1, 1)),
            });
        }
        assert_eq!(slot.take().unwrap().seq, 3);
        assert!(slot.take().is_none());
        slot.fail(CameraError::NoCamera);
        assert_eq!(slot.error(), Some(CameraError::NoCamera));
    }

    #[test]
    fn camera_formats_convert_to_rgb() {
        // Mid grey and pure red, as YUV.
        let grey = yuyv_to_rgb(&[126, 128, 126, 128], 2, 1).unwrap();
        assert!(
            grey.pixels()
                .all(|p| p.0.iter().all(|&c| c.abs_diff(128) <= 2))
        );
        let red = nv12_to_rgb(&[81, 81, 81, 81, 90, 240], 2, 2).unwrap();
        let p = red.get_pixel(1, 1).0;
        assert!(p[0] > 240 && p[1] < 10 && p[2] < 10, "{p:?}");
        assert!(yuyv_to_rgb(&[0; 3], 2, 1).is_none());
        let big = image::RgbImage::from_pixel(1280, 720, image::Rgb([10, 20, 30]));
        let small = shrink(big, 640);
        assert_eq!(small.dimensions(), (640, 360));
        assert_eq!(small.get_pixel(5, 5).0, [10, 20, 30]);
    }
}
