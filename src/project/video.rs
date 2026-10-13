//! Writing a sequence of pictures as a video or an animation: GIF and
//! animated PNG here, MP4 and WebM through `ffmpeg` (if it's installed),
//! or numbered PNG files. Used by the time-lapse and the animation export.

use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VideoFormat {
    Gif,
    /// Animated PNG: full colour and transparency, played by browsers.
    Apng,
    /// H.264, through ffmpeg.
    Mp4,
    /// VP9 (with transparency), through ffmpeg.
    WebM,
    /// A PNG for each frame, numbered.
    PngSequence,
}

impl VideoFormat {
    pub fn label(self) -> &'static str {
        match self {
            VideoFormat::Gif => "GIF",
            VideoFormat::Apng => "Animated PNG",
            VideoFormat::Mp4 => "MP4 (needs ffmpeg)",
            VideoFormat::WebM => "WebM (needs ffmpeg)",
            VideoFormat::PngSequence => "PNG sequence",
        }
    }

    #[cfg(test)]
    pub fn extension(self) -> &'static str {
        match self {
            VideoFormat::Gif => "gif",
            VideoFormat::Apng | VideoFormat::PngSequence => "png",
            VideoFormat::Mp4 => "mp4",
            VideoFormat::WebM => "webm",
        }
    }

    /// Whether frames keep their transparency (else they're on white).
    pub fn has_alpha(self) -> bool {
        !matches!(self, VideoFormat::Mp4)
    }

    /// From a file name's extension (`.png` is an animated PNG).
    #[cfg_attr(mobile, allow(dead_code))]
    pub fn from_path(path: &Path) -> Option<Self> {
        let e = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match e.as_str() {
            "gif" => VideoFormat::Gif,
            "png" | "apng" => VideoFormat::Apng,
            "mp4" | "m4v" => VideoFormat::Mp4,
            "webm" => VideoFormat::WebM,
            _ => return None,
        })
    }
}

/// One frame: unmultiplied RGBA, `width`×`height`.
pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl VideoFrame {
    /// The frame over white (videos without transparency).
    fn on_white(mut self) -> Self {
        for px in self.rgba.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            for c in &mut px[..3] {
                *c = ((*c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
            }
            px[3] = 255;
        }
        self
    }
}

/// Write `frames` (all the same size) as `format` to `path`, played at `fps`.
/// `loops`: play forever (GIF and APNG). Returns where it went (a PNG
/// sequence's folder).
pub fn write_video(
    path: &Path,
    format: VideoFormat,
    fps: u32,
    loops: bool,
    frames: impl Iterator<Item = VideoFrame>,
) -> Result<PathBuf, String> {
    let fps = fps.clamp(1, 120);
    let frames = frames.map(|f| if format.has_alpha() { f } else { f.on_white() });
    match format {
        VideoFormat::Gif => write_gif(path, fps, loops, frames).map(|_| path.into()),
        VideoFormat::Apng => write_apng(path, fps, loops, frames.collect()).map(|_| path.into()),
        VideoFormat::Mp4 | VideoFormat::WebM => {
            write_ffmpeg(path, format, fps, frames).map(|_| path.into())
        }
        VideoFormat::PngSequence => write_sequence(path, frames),
    }
}

fn write_gif(
    path: &Path,
    fps: u32,
    loops: bool,
    frames: impl Iterator<Item = VideoFrame>,
) -> Result<(), String> {
    use image::codecs::gif::{GifEncoder, Repeat};
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = GifEncoder::new_with_speed(std::io::BufWriter::new(file), 20);
    let repeat = if loops {
        Repeat::Infinite
    } else {
        Repeat::Finite(0)
    };
    encoder.set_repeat(repeat).map_err(|e| e.to_string())?;
    let delay = image::Delay::from_numer_denom_ms(1000, fps);
    for f in frames {
        let img = image::RgbaImage::from_raw(f.width as u32, f.height as u32, f.rgba)
            .ok_or("Bad frame")?;
        encoder
            .encode_frame(image::Frame::from_parts(img, 0, 0, delay))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn write_apng(path: &Path, fps: u32, loops: bool, frames: Vec<VideoFrame>) -> Result<(), String> {
    let first = frames.first().ok_or("No frames")?;
    let (w, h) = (first.width as u32, first.height as u32);
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let err = |e: png::EncodingError| e.to_string();
    encoder
        .set_animated(frames.len() as u32, if loops { 0 } else { 1 })
        .map_err(err)?;
    encoder.set_frame_delay(1, fps as u16).map_err(err)?;
    let mut writer = encoder.write_header().map_err(err)?;
    for f in &frames {
        writer.write_image_data(&f.rgba).map_err(err)?;
    }
    writer.finish().map_err(err)
}

fn write_sequence(
    path: &Path,
    frames: impl Iterator<Item = VideoFrame>,
) -> Result<PathBuf, String> {
    // `name.png` → a folder `name/` of `name_0001.png`...
    let stem = path
        .file_stem()
        .map_or_else(|| "frame".into(), |s| s.to_string_lossy().into_owned());
    let dir = path.with_extension("");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (i, f) in frames.enumerate() {
        let img = image::RgbaImage::from_raw(f.width as u32, f.height as u32, f.rgba)
            .ok_or("Bad frame")?;
        img.save(dir.join(format!("{stem}_{:04}.png", i + 1)))
            .map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

fn write_ffmpeg(
    path: &Path,
    format: VideoFormat,
    fps: u32,
    frames: impl Iterator<Item = VideoFrame>,
) -> Result<(), String> {
    let mut frames = frames.peekable();
    let first = frames.peek().ok_or("No frames")?;
    let size = format!("{}x{}", first.width, first.height);
    let mut command = std::process::Command::new("ffmpeg");
    command
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
        ])
        .args(["-s", &size, "-r", &fps.to_string(), "-i", "-"]);
    match format {
        VideoFormat::WebM => command.args([
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuva420p",
            "-b:v",
            "0",
            "-crf",
            "30",
        ]),
        // Even sides, as H.264 needs.
        _ => command
            .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2:color=white"])
            .args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ]),
    };
    let mut child = command
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| {
            format!(
                "{} needs ffmpeg installed (or export as GIF)",
                format.label()
            )
        })?;
    let mut stdin = child.stdin.take().ok_or("Couldn't start ffmpeg")?;
    for f in frames {
        if stdin.write_all(&f.rgba).is_err() {
            break; // ffmpeg stopped; its error says why
        }
    }
    drop(stdin);
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize) -> impl Iterator<Item = VideoFrame> {
        (0..n).map(|i| VideoFrame {
            width: 6,
            height: 4,
            rgba: (0..24)
                .flat_map(|p| {
                    [
                        (i * 40) as u8,
                        p as u8 * 10,
                        0,
                        if p % 2 == 0 { 255 } else { 0 },
                    ]
                })
                .collect(),
        })
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rp-video-{}-{name}", std::process::id()))
    }

    #[test]
    fn gif_and_apng_hold_every_frame() {
        use image::AnimationDecoder;
        let gif = temp("a.gif");
        write_video(&gif, VideoFormat::Gif, 12, true, frames(5)).unwrap();
        let decoder = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(
            std::fs::File::open(&gif).unwrap(),
        ))
        .unwrap();
        let decoded = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(decoded.len(), 5);
        // (GIF keeps delays in hundredths: 1/12 s is 80 ms.)
        let (n, d) = decoded[0].delay().numer_denom_ms();
        assert_eq!(n / d, 80);
        let apng = temp("a.png");
        write_video(&apng, VideoFormat::Apng, 8, true, frames(3)).unwrap();
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&apng).unwrap()));
        let reader = decoder.read_info().unwrap();
        let control = reader.info().animation_control().unwrap();
        assert_eq!((control.num_frames, control.num_plays), (3, 0));
        let _ = std::fs::remove_file(gif);
        let _ = std::fs::remove_file(apng);
    }

    #[test]
    fn a_png_sequence_is_a_folder_of_numbered_pictures() {
        let path = temp("seq.png");
        let dir = write_video(&path, VideoFormat::PngSequence, 12, true, frames(3)).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            names,
            (1..=3)
                .map(|i| format!("{stem}_{i:04}.png"))
                .collect::<Vec<_>>()
        );
        let img = image::open(dir.join(&names[1])).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(0, 0).0, [40, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 0).0[3], 0, "transparency kept");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mp4_and_webm_go_through_ffmpeg_when_it_is_there() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("no ffmpeg; skipping");
            return;
        }
        for format in [VideoFormat::Mp4, VideoFormat::WebM] {
            let path = temp(&format!("v.{}", format.extension()));
            write_video(&path, format, 12, true, frames(4)).unwrap();
            assert!(
                std::fs::metadata(&path).unwrap().len() > 100,
                "{}",
                format.label()
            );
            let _ = std::fs::remove_file(path);
        }
    }
}
