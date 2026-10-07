//! Time-lapse recording: while it's on, a small picture of the canvas is
//! kept after each change, and File → Export Time-lapse turns them into a
//! video (MP4, through `ffmpeg` if it's installed) or an animated GIF.
//!
//! Frames live in memory for the session (not in the project file), each
//! at most [`FRAME_EDGE`] px on its long side and zstd-compressed. Past
//! [`MAX_FRAMES`], every other frame is dropped: the video still covers the
//! whole session, just faster.

use crate::PainterApp;
use eframe::egui::{Color32, ColorImage};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Long side of a recorded frame.
const FRAME_EDGE: usize = 720;
/// Frames kept before thinning.
const MAX_FRAMES: usize = 1500;
/// At most one frame per this long (a burst of quick steps is one frame).
const MIN_GAP: Duration = Duration::from_millis(500);
/// Playback speed.
const FPS: u32 = 30;

struct Frame {
    width: usize,
    height: usize,
    /// Unmultiplied RGBA, zstd-compressed.
    rgba_zstd: Vec<u8>,
}

#[derive(Default)]
pub struct TimelapseState {
    pub recording: bool,
    frames: Vec<Frame>,
    /// Where the history stood at the last frame.
    last_version: Option<(u64, usize, usize)>,
    last_capture: Option<Instant>,
    /// The export running.
    pub task: Option<std::thread::JoinHandle<Result<String, String>>>,
}

impl TimelapseState {
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

impl PainterApp {
    /// Once a frame: record the canvas if it changed.
    pub(crate) fn timelapse_tick(&mut self) {
        if let Some(task) = self.workspace.timelapse.task.take_if(|t| t.is_finished()) {
            let result = task
                .join()
                .unwrap_or_else(|_| Err("The export stopped".into()));
            self.export_state.message = Some(result.unwrap_or_else(|e| e));
        }
        let state = &self.workspace.timelapse;
        if !state.recording || self.brush_state.is_drawing {
            return;
        }
        let version = self.doc_version();
        if state.last_version == Some(version)
            || state.last_capture.is_some_and(|t| t.elapsed() < MIN_GAP)
        {
            return;
        }
        self.timelapse_capture();
    }

    fn timelapse_capture(&mut self) {
        let img = self.canvas_thumbnail();
        let [fw, fh] = img.size;
        let raw: Vec<u8> = img
            .pixels
            .iter()
            .flat_map(|&p| crate::canvas::blend::unmultiply(p))
            .collect();
        let version = self.doc_version();
        let state = &mut self.workspace.timelapse;
        state.last_version = Some(version);
        state.last_capture = Some(Instant::now());
        let Ok(rgba_zstd) = zstd::bulk::compress(&raw, 1) else {
            return;
        };
        state.frames.push(Frame {
            width: fw,
            height: fh,
            rgba_zstd,
        });
        if state.frames.len() > MAX_FRAMES {
            // Keep every other frame (and always the newest).
            let last = state.frames.len() - 1;
            let mut i = 0;
            state.frames.retain(|_| {
                let keep = i % 2 == 0 || i == last;
                i += 1;
                keep
            });
        }
    }

    /// The whole canvas shrunk by a power of two to at most about
    /// [`FRAME_EDGE`] px, tile by tile in parallel through the zoomed-out
    /// compositor (the same pixels the view shows at that zoom).
    fn canvas_thumbnail(&self) -> ColorImage {
        use rayon::prelude::*;
        let c = &self.canvas;
        let (w, h, ts) = (c.width(), c.height(), c.tile_size());
        let block = w.max(h).div_ceil(FRAME_EDGE).next_power_of_two().min(ts);
        let (fw, fh) = (w.div_ceil(block), h.div_ceil(block));
        let tiles: Vec<(usize, usize)> = (0..h.div_ceil(ts))
            .flat_map(|ty| (0..w.div_ceil(ts)).map(move |tx| (tx, ty)))
            .collect();
        let parts: Vec<ColorImage> = self.workspace.pool.install(|| {
            tiles
                .par_iter()
                .map(|&(tx, ty)| {
                    let rect = [0, 0, ts.min(w - tx * ts), ts.min(h - ty * ts)];
                    let mut part = ColorImage::new([0, 0], Color32::TRANSPARENT);
                    c.write_tile_rect_downsampled(tx, ty, rect, block, &mut part, None);
                    part
                })
                .collect()
        });
        let mut img = ColorImage::new([fw, fh], Color32::TRANSPARENT);
        let per_tile = ts / block;
        for (&(tx, ty), part) in tiles.iter().zip(&parts) {
            let [pw, ph] = part.size;
            for row in 0..ph {
                let dst = (ty * per_tile + row) * fw + tx * per_tile;
                img.pixels[dst..dst + pw].copy_from_slice(&part.pixels[row * pw..(row + 1) * pw]);
            }
        }
        img
    }

    /// Turn recording on or off. Turning it on starts from the canvas as
    /// it is.
    pub(crate) fn set_timelapse_recording(&mut self, on: bool) {
        self.workspace.timelapse.recording = on;
        if on {
            self.workspace.timelapse.last_version = None;
            self.workspace.timelapse.last_capture = None;
            self.timelapse_capture();
        }
    }

    pub(crate) fn clear_timelapse(&mut self) {
        let state = &mut self.workspace.timelapse;
        state.frames.clear();
        state.last_version = None;
    }

    /// Export the recording to `path` (`.mp4` or `.gif`) on a worker thread.
    pub(crate) fn export_timelapse(&mut self, path: PathBuf) {
        // The canvas as it is now closes the video.
        if self.workspace.timelapse.recording {
            self.workspace.timelapse.last_version = None;
            self.workspace.timelapse.last_capture = None;
            self.timelapse_capture();
        }
        let state = &mut self.workspace.timelapse;
        if state.frames.len() < 2 || state.task.is_some() {
            self.export_state.message = Some(if state.task.is_some() {
                "A time-lapse export is already running".into()
            } else {
                "Nothing recorded yet: turn on File → Record Time-lapse".into()
            });
            return;
        }
        let frames: Vec<Frame> = state
            .frames
            .iter()
            .map(|f| Frame {
                width: f.width,
                height: f.height,
                rgba_zstd: f.rgba_zstd.clone(),
            })
            .collect();
        self.export_state.message = Some("Exporting the time-lapse…".into());
        state.task = Some(std::thread::spawn(move || {
            write_frames(&frames, &path)?;
            // Android: from the cache into Pictures.
            #[cfg(target_os = "android")]
            return crate::android::publish_file(&path, "image/gif").map(|done| done.message);
            #[cfg(not(target_os = "android"))]
            Ok(format!("Time-lapse saved to {}", path.display()))
        }));
    }
}

/// The recording as a GIF (by `path`'s extension) or an MP4.
fn write_frames(frames: &[Frame], path: &std::path::Path) -> Result<(), String> {
    use crate::project::video::{VideoFormat, VideoFrame, write_video};
    let gif = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    let format = if gif {
        VideoFormat::Gif
    } else {
        VideoFormat::Mp4
    };
    let video = video_frames(frames).map(|(width, height, rgba)| VideoFrame {
        width,
        height,
        rgba,
    });
    write_video(path, format, FPS, true, video).map(|_| ())
}

/// Every frame at the last frame's size (earlier ones fitted into it on
/// white, should the canvas have been resized), with the last one held for
/// a second at the end.
fn video_frames(frames: &[Frame]) -> impl Iterator<Item = (usize, usize, Vec<u8>)> + '_ {
    let last = frames.last().expect("at least two frames");
    let (w, h) = (last.width, last.height);
    let hold = FPS as usize;
    frames
        .iter()
        .chain(std::iter::repeat_n(last, hold))
        .filter_map(move |f| {
            let raw = zstd::bulk::decompress(&f.rgba_zstd, f.width * f.height * 4).ok()?;
            let img = image::RgbaImage::from_raw(f.width as u32, f.height as u32, raw)?;
            let img = if (f.width, f.height) == (w, h) {
                img
            } else {
                let scale = (w as f32 / f.width as f32).min(h as f32 / f.height as f32);
                let (sw, sh) = (
                    ((f.width as f32 * scale) as u32).max(1),
                    ((f.height as f32 * scale) as u32).max(1),
                );
                let small =
                    image::imageops::resize(&img, sw, sh, image::imageops::FilterType::Triangle);
                let mut out =
                    image::RgbaImage::from_pixel(w as u32, h as u32, image::Rgba([255; 4]));
                image::imageops::overlay(
                    &mut out,
                    &small,
                    ((w as u32 - sw) / 2).into(),
                    ((h as u32 - sh) / 2).into(),
                );
                out
            };
            // On white: videos have no transparency.
            let mut rgb = img.into_raw();
            for px in rgb.as_chunks_mut::<4>().0 {
                let a = px[3] as u32;
                for c in &mut px[..3] {
                    *c = ((*c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
                }
                px[3] = 255;
            }
            Some((w, h, rgb))
        })
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::history::UndoAction;
    use eframe::egui::Color32;

    fn step(app: &mut crate::PainterApp) {
        app.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        });
        app.workspace.timelapse.last_capture = None; // no waiting in tests
        app.timelapse_tick();
    }

    #[test]
    fn a_frame_per_change_while_recording() {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(1500, 900, Color32::WHITE, 64));
        app.timelapse_tick();
        assert_eq!(app.workspace.timelapse.frame_count(), 0, "off by default");
        app.set_timelapse_recording(true);
        assert_eq!(
            app.workspace.timelapse.frame_count(),
            1,
            "starts from the canvas"
        );
        app.workspace.timelapse.last_capture = None;
        app.timelapse_tick();
        assert_eq!(app.workspace.timelapse.frame_count(), 1, "nothing changed");
        step(&mut app);
        step(&mut app);
        assert_eq!(app.workspace.timelapse.frame_count(), 3);
        let f = &app.workspace.timelapse.frames[0];
        assert!(f.width <= super::FRAME_EDGE && f.height <= super::FRAME_EDGE);
    }

    #[test]
    fn long_recordings_are_thinned_not_cut() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.set_timelapse_recording(true);
        for _ in 0..super::MAX_FRAMES + 5 {
            step(&mut app);
        }
        let n = app.workspace.timelapse.frame_count();
        assert!(
            n <= super::MAX_FRAMES && n > super::MAX_FRAMES / 2 - 10,
            "{n}"
        );
    }

    #[test]
    fn exports_a_gif_and_an_mp4() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(90, 50, Color32::WHITE, 64));
        app.set_timelapse_recording(true);
        app.canvas_mut()
            .set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        step(&mut app);
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let gif = dir.join(format!("rp-timelapse-{pid}.gif"));
        let frames = std::mem::take(&mut app.workspace.timelapse.frames);
        super::write_frames(&frames, &gif).unwrap();
        let decoded = image::open(&gif).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (90, 50));
        let _ = std::fs::remove_file(&gif);
        // MP4 only where ffmpeg is installed.
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok()
        {
            let mp4 = dir.join(format!("rp-timelapse-{pid}.mp4"));
            super::write_frames(&frames, &mp4).unwrap();
            assert!(std::fs::metadata(&mp4).unwrap().len() > 100);
            let _ = std::fs::remove_file(&mp4);
        }
    }
}
