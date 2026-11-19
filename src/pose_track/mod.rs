//! Posing figures from the camera and from photos: on-device hand and
//! body tracking (feature `pose-capture`). The models run with tract on a
//! worker thread, which always takes the newest frame (older ones waiting
//! are dropped), so a slow device skips frames rather than falling behind.
//! Nothing leaves the device.
//!
//! The models ship beside the program like the figure pack
//! (`assets/pose-models/v1`, in the APK's assets on Android, copied into
//! the data folder on first use) and are loaded once, when first wanted.
//! A build without them downloads them into the data folder then (their
//! manifest, built in, gives each one's URL and SHA-256).

pub mod body;
mod crop;
pub mod hand;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

pub use body::{BodyLandmarks, BodyModels, BodyTracker};
pub use hand::{HandLandmarks, HandModels, HandTracker, Handedness};

/// The models' id, and their folder in the APK's assets.
pub const PACK_ID: &str = "pose-models-v1";
/// Their folder in a `pose-models` folder (the data folder's, or beside
/// the program).
pub const PACK_DIR: &str = "v1";
const VERSION: u32 = 1;

/// What the app says when the models aren't there.
pub const NOT_INSTALLED: &str = "The pose tracking models aren't installed";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelFile {
    pub file: String,
    /// Its licence (SPDX), and where it came from.
    pub licence: String,
    pub source: String,
    /// Its SHA-256 (hex), as downloaded.
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    pub models: Vec<ModelFile>,
    /// The CRC-32 (hex) of the model files, one after the other in the
    /// manifest's order: a new set is a new manifest, so Android unpacks
    /// it again. Keep it in step with the files (a test checks).
    #[serde(default)]
    pub content: String,
}

/// The manifest in `root`, checked.
pub fn manifest(root: &Path) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if manifest.version != VERSION || manifest.id != PACK_ID {
        return Err("Unsupported pose tracking models".into());
    }
    if (manifest.models.iter()).any(|m| Path::new(&m.file).components().count() != 1) {
        return Err("Invalid pose tracking model file".into());
    }
    Ok(manifest)
}

/// The stamp of the model files (see [`Manifest::content`]).
#[cfg(test)]
pub fn content_stamp(root: &Path, manifest: &Manifest) -> Result<String, String> {
    let mut crc = crc32fast::Hasher::new();
    for model in &manifest.models {
        crc.update(&std::fs::read(root.join(&model.file)).map_err(|e| e.to_string())?);
    }
    Ok(format!("{:08x}", crc.finalize()))
}

/// The model files, in the manifest's order.
const FILES: [&str; 4] = [
    hand::PALM_FILE,
    hand::LANDMARK_FILE,
    body::DETECT_FILE,
    body::LANDMARK_FILE,
];

/// Whether `root` holds the models (not just Git LFS pointers to them, as
/// in a checkout without LFS).
pub fn is_installed(root: &Path) -> bool {
    manifest(root).is_ok_and(|m| {
        FILES.iter().all(|f| {
            m.models.iter().any(|model| model.file == *f)
                && std::fs::read(root.join(f)).is_ok_and(|bytes| {
                    bytes.len() > 1024 && !bytes.starts_with(b"version https://git-lfs")
                })
        })
    })
}

/// Where the models may be, in order: the data folder's `pose-models`
/// folder (on Android the APK's are copied there first), the one beside
/// the program, and in builds for development the repository's.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !cfg!(test) {
        let own = crate::app::init::data_dir()
            .join("pose-models")
            .join(PACK_DIR);
        #[cfg(target_os = "android")]
        if let Err(err) = crate::android::extract_pose_models(&own) {
            log::warn!("Couldn't unpack the pose tracking models: {err}");
        }
        out.push(own);
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("pose-models").join(PACK_DIR)))
        {
            out.push(dir);
        }
    }
    if cfg!(debug_assertions) {
        out.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/pose-models")
                .join(PACK_DIR),
        );
    }
    out
}

/// The installed models' folder (looked for until found).
pub fn installed() -> Option<PathBuf> {
    static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut dir = DIR.lock().ok()?;
    if dir.is_none() {
        *dir = candidates().into_iter().find(|d| is_installed(d));
    }
    dir.clone()
}

/// The models' folder, downloading them first if they aren't installed
/// (a build without them, or a checkout without Git LFS). Slow: for a
/// worker's or a job's thread.
fn models_dir() -> Result<PathBuf, String> {
    if let Some(dir) = installed() {
        return Ok(dir);
    }
    if cfg!(test) {
        return Err(NOT_INSTALLED.into());
    }
    download(
        &crate::app::init::data_dir()
            .join("pose-models")
            .join(PACK_DIR),
    )?;
    installed().ok_or_else(|| NOT_INSTALLED.to_string())
}

/// Bytes downloaded so far while the models download (None otherwise),
/// for the windows to show.
pub fn downloading() -> Option<u64> {
    DOWNLOADING
        .load(Ordering::Relaxed)
        .then(|| DOWNLOADED.load(Ordering::Relaxed))
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static DOWNLOADED: AtomicU64 = AtomicU64::new(0);

/// Download the models the shipped manifest lists into `dest`, each
/// checked against its SHA-256, the manifest written last (so a folder
/// half done isn't taken as installed).
fn download(dest: &Path) -> Result<(), String> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().map_err(|e| e.to_string())?;
    if is_installed(dest) {
        return Ok(());
    }
    const MANIFEST: &str = include_str!("../../assets/pose-models/v1/manifest.json");
    const LICENCE: &str = include_str!("../../assets/pose-models/v1/LICENSE");
    let manifest: Manifest = serde_json::from_str(MANIFEST).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    DOWNLOADED.store(0, Ordering::Relaxed);
    DOWNLOADING.store(true, Ordering::Relaxed);
    let result = manifest.models.iter().try_for_each(|model| {
        let path = dest.join(&model.file);
        if std::fs::read(&path).is_ok_and(|b| hex(&sha256(&b)) == model.sha256) {
            return Ok(());
        }
        // The URL ends the source's description.
        let url = (model.source.rfind("https://")).map(|i| &model.source[i..]);
        let url = url.ok_or_else(|| format!("No download for {}", model.file))?;
        log::info!("Downloading {url}");
        let bytes = fetch(url).map_err(|e| format!("Couldn't download {}: {e}", model.file))?;
        if hex(&sha256(&bytes)) != model.sha256 {
            return Err(format!("{} downloaded isn't the one expected", model.file));
        }
        let part = path.with_extension("part");
        std::fs::write(&part, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&part, &path).map_err(|e| e.to_string())
    });
    DOWNLOADING.store(false, Ordering::Relaxed);
    result?;
    std::fs::write(dest.join("LICENSE"), LICENCE).map_err(|e| e.to_string())?;
    std::fs::write(dest.join("manifest.json"), MANIFEST).map_err(|e| e.to_string())
}

/// The body at `url`, counted into [`DOWNLOADED`] as it comes.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let response = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buf[..n]);
        DOWNLOADED.fetch_add(n as u64, Ordering::Relaxed);
    }
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hand models, loaded once (later calls share them; a failure is
/// tried again next time).
pub fn hand_models() -> Result<Arc<HandModels>, String> {
    static MODELS: OnceLock<Arc<HandModels>> = OnceLock::new();
    if let Some(models) = MODELS.get() {
        return Ok(models.clone());
    }
    let models = Arc::new(HandModels::load(&models_dir()?)?);
    Ok(MODELS.get_or_init(|| models).clone())
}

/// The body models, loaded once (later calls share them; a failure is
/// tried again next time).
pub fn body_models() -> Result<Arc<BodyModels>, String> {
    static MODELS: OnceLock<Arc<BodyModels>> = OnceLock::new();
    if let Some(models) = MODELS.get() {
        return Ok(models.clone());
    }
    let models = Arc::new(BodyModels::load(&models_dir()?)?);
    Ok(MODELS.get_or_init(|| models).clone())
}

/// The bodies in a photo, the biggest first (and their hands, if
/// `hands`), with the installed models: for a job's thread, it takes a
/// while.
pub fn read_photo(image: &image::RgbImage, hands: bool) -> Result<Vec<BodyLandmarks>, String> {
    let models = body_models()?;
    let mut bodies = models.read_photo(image)?;
    if hands {
        let hand_models = hand_models()?;
        for body in &mut bodies {
            BodyTracker::default().track_hands(&hand_models, image, body)?;
        }
    }
    Ok(bodies)
}

/// A frame for the tracker: its picture, and a number that grows frame to
/// frame (the camera's).
#[derive(Clone)]
pub struct Frame {
    pub seq: u64,
    pub image: Arc<image::RgbImage>,
}

/// What the tracker looks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrackMode {
    /// One hand.
    #[default]
    Hand,
    /// A body, and its hands read again at the wrists if `hands`.
    Body { hands: bool },
}

/// What the tracker made of a frame.
#[derive(Clone, Debug)]
pub struct Tracked {
    /// What it looked for.
    pub mode: TrackMode,
    /// The hand in it, if one was found (looking for a hand).
    pub hand: Option<HandLandmarks>,
    /// The body in it, if one was found (looking for a body).
    pub body: Option<BodyLandmarks>,
    /// How long the frame took, in milliseconds.
    pub millis: f32,
}

/// The worker's mailbox: the newest frame waiting, what to look for, the
/// newest result.
#[derive(Default)]
struct Shared {
    frame: Option<Frame>,
    mode: TrackMode,
    result: Option<Result<Tracked, String>>,
    stop: bool,
}

/// What loads a set of models.
type Loader<T> = Box<dyn FnOnce() -> Result<Arc<T>, String> + Send>;

/// Models loaded when first wanted.
struct Lazy<T> {
    load: Option<Loader<T>>,
    models: Option<Result<Arc<T>, String>>,
}

impl<T> Lazy<T> {
    fn new(load: impl FnOnce() -> Result<Arc<T>, String> + Send + 'static) -> Self {
        Self {
            load: Some(Box::new(load)),
            models: None,
        }
    }

    fn get(&mut self) -> Result<Arc<T>, String> {
        let load = &mut self.load;
        self.models
            .get_or_insert_with(|| match load.take() {
                Some(load) => load(),
                None => Err(NOT_INSTALLED.into()),
            })
            .clone()
    }
}

/// Hand or body tracking on a thread of its own. Frames handed to it
/// replace any still waiting; dropping it stops the thread.
pub struct PoseWorker {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PoseWorker {
    /// Start a worker; it loads the models it needs (once for the app)
    /// when it first needs them.
    pub fn start() -> Self {
        Self::with_models(hand_models, body_models)
    }

    /// A worker using the models `hand` and `body` give.
    pub fn with_models(
        hand: impl FnOnce() -> Result<Arc<HandModels>, String> + Send + 'static,
        body: impl FnOnce() -> Result<Arc<BodyModels>, String> + Send + 'static,
    ) -> Self {
        let shared: Arc<(Mutex<Shared>, Condvar)> = Arc::default();
        let inbox = shared.clone();
        let (hand, body) = (Lazy::new(hand), Lazy::new(body));
        let thread = std::thread::Builder::new()
            .name("pose tracking".into())
            .spawn(move || worker(inbox, hand, body))
            .ok();
        Self { shared, thread }
    }

    /// A worker without models (tests).
    #[cfg(test)]
    pub fn without_models() -> Self {
        Self::with_models(|| Err(NOT_INSTALLED.into()), || Err(NOT_INSTALLED.into()))
    }

    /// Look for `mode` from the next frame on.
    pub fn set_mode(&self, mode: TrackMode) {
        let (lock, _) = &*self.shared;
        lock.lock().unwrap_or_else(|e| e.into_inner()).mode = mode;
    }

    /// Hand a frame to the worker (replacing one it hasn't started).
    pub fn submit(&self, frame: Frame) {
        let (lock, wake) = &*self.shared;
        lock.lock().unwrap_or_else(|e| e.into_inner()).frame = Some(frame);
        wake.notify_one();
    }

    /// The newest result, taken (None if there's no new one).
    pub fn take(&self) -> Option<Result<Tracked, String>> {
        let (lock, _) = &*self.shared;
        lock.lock().unwrap_or_else(|e| e.into_inner()).result.take()
    }

    /// Wait up to `timeout` for a result (tests).
    #[cfg(test)]
    pub fn wait(&self, timeout: std::time::Duration) -> Option<Result<Tracked, String>> {
        let end = std::time::Instant::now() + timeout;
        loop {
            if let Some(r) = self.take() {
                return Some(r);
            }
            if std::time::Instant::now() > end {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

impl Drop for PoseWorker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        lock.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn worker(
    shared: Arc<(Mutex<Shared>, Condvar)>,
    mut hand_models: Lazy<HandModels>,
    mut body_models: Lazy<BodyModels>,
) {
    let (lock, wake) = &*shared;
    let mut hands = HandTracker::default();
    let mut bodies = BodyTracker::default();
    let mut last: Option<(u64, TrackMode)> = None;
    loop {
        let (frame, mode) = {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.stop {
                    return;
                }
                if let Some(frame) = state.frame.take() {
                    break (frame, state.mode);
                }
                state = wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        // A frame from before the last (a new camera), or something else
        // to look for, starts over.
        if last.is_some_and(|(seq, was)| frame.seq <= seq || was != mode) {
            hands.reset();
            bodies.reset();
        }
        last = Some((frame.seq, mode));
        let start = std::time::Instant::now();
        let result = match mode {
            TrackMode::Hand => hand_models
                .get()
                .and_then(|models| hands.track(&models, &frame.image))
                .map(|hand| (hand, None)),
            TrackMode::Body { hands: with_hands } => body_models
                .get()
                .and_then(|models| bodies.track(&models, &frame.image))
                .and_then(|body| match body {
                    Some(mut body) if with_hands => {
                        let models = hand_models.get()?;
                        bodies.track_hands(&models, &frame.image, &mut body)?;
                        Ok((None, Some(body)))
                    }
                    body => Ok((None, body)),
                }),
        }
        .map(|(hand, body)| Tracked {
            mode,
            hand,
            body,
            millis: start.elapsed().as_secs_f32() * 1000.0,
        });
        lock.lock().unwrap_or_else(|e| e.into_inner()).result = Some(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network: `cargo test downloads_the_models -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_the_models() {
        let dest = std::env::temp_dir().join("rusty-painter-pose-models-test");
        let _ = std::fs::remove_dir_all(&dest);
        download(&dest).unwrap();
        assert!(is_installed(&dest));
        assert_eq!(manifest(&dest).unwrap(), manifest(&shipped()).unwrap());
        std::fs::remove_dir_all(&dest).unwrap();
    }

    fn shipped() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pose-models/v1")
    }

    #[test]
    fn the_shipped_manifest_lists_the_models_under_a_free_licence() {
        let m = manifest(&shipped()).unwrap();
        let files: Vec<&str> = m.models.iter().map(|f| f.file.as_str()).collect();
        assert_eq!(files, FILES);
        for model in &m.models {
            assert_eq!(model.licence, "Apache-2.0");
            assert_eq!(model.sha256.len(), 64);
            assert!(!model.source.is_empty());
        }
        assert!(shipped().join("LICENSE").is_file());
    }

    #[test]
    fn the_shipped_models_stamp_matches_their_files() {
        let root = shipped();
        if !is_installed(&root) {
            eprintln!("the pose models aren't pulled from Git LFS; skipping");
            return;
        }
        let m = manifest(&root).unwrap();
        assert_eq!(
            m.content,
            content_stamp(&root, &m).unwrap(),
            "update the manifest's content after changing the models"
        );
    }

    #[test]
    fn a_worker_without_models_says_so_and_stops_when_dropped() {
        let worker = PoseWorker::without_models();
        worker.submit(Frame {
            seq: 1,
            image: Arc::new(image::RgbImage::new(8, 8)),
        });
        let result = worker.wait(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(result.unwrap_err(), NOT_INSTALLED);
        drop(worker);
    }

    #[test]
    fn the_worker_tracks_the_photo() {
        let root = shipped();
        if !is_installed(&root) {
            return;
        }
        let body_root = root.clone();
        let worker = PoseWorker::with_models(
            move || HandModels::load(&root).map(Arc::new),
            move || BodyModels::load(&body_root).map(Arc::new),
        );
        let open = |name: &str| {
            Arc::new(
                image::open(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("testdata")
                        .join(name),
                )
                .unwrap()
                .to_rgb8(),
            )
        };
        worker.submit(Frame {
            seq: 1,
            image: open("hand-photo.jpg"),
        });
        let tracked = worker
            .wait(std::time::Duration::from_secs(60))
            .unwrap()
            .unwrap();
        assert!(tracked.hand.is_some() && tracked.body.is_none());
        // Then a body.
        worker.set_mode(TrackMode::Body { hands: true });
        worker.submit(Frame {
            seq: 2,
            image: open("body-photo.jpg"),
        });
        let tracked = worker
            .wait(std::time::Duration::from_secs(60))
            .unwrap()
            .unwrap();
        assert_eq!(tracked.mode, TrackMode::Body { hands: true });
        assert!(tracked.hand.is_none());
        let body = tracked.body.expect("the body is found");
        assert!(body.confidence > 0.9);
        eprintln!("body and hands: {} ms", tracked.millis);
    }
}
