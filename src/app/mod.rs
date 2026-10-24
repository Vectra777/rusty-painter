//! The application: one `PainterApp` owning the document, the tools and
//! the UI state, updated once a frame by egui.
//!
//! - [`painter`]: the `PainterApp` type and the frame loop (`update`).
//! - [`state`]: the app state groups `PainterApp` is made of.
//! - [`document`]: document settings, limits and tile types.
//! - [`clipboard`]: copy, cut, paste and duplicating layers.
//! - [`canvas_ops`], [`stroke_ops`]: operations on the document (new canvas,
//!   layers, brush strokes through the stroke worker).
//! - [`input`]: pointer, pen, touch and keyboard input, routed to tools.
//! - [`tools`]: one module per tool (selection, fill, gradient, shapes...).
//! - [`view`]: drawing the canvas (GPU atlases) and the screen mapping.
//! - [`frame_stats`]: how long each stage of a frame takes (the readout).
//! - [`pressure_calibration`]: fitting the pen pressure curve to test strokes.
//! - [`timelapse`]: recording the painting and exporting it as a video.
//! - [`collab`]: painting together on one canvas over the network.
//! - [`color_management`]: the monitor's profile, proofing, assigning and
//!   converting the document's profile.
//! - [`animation`]: frames, playback, drawings and exporting animations.
//! - [`rig`]: rig layers, importing animations and videos.
//! - [`settings`]: preferences and tool options kept between sessions.
//! - [`autosave`]: autosave and recovering unsaved work after a crash.
//! - [`shader_ops`]: shader layers (compiling, playback, baking).
//! - [`jobs`]: slow work (files, dialogs, decoding) off the UI thread.
//! - [`layout`], [`init`], [`import`], [`brush_io`]: docks, startup, files
//!   dropped or imported, brush tips on disk.
pub(crate) mod animation;
pub(crate) mod autosave;
pub(crate) mod brush_io;
pub(crate) mod brush_library;
pub(crate) mod canvas_ops;
pub(crate) mod clipboard;
pub(crate) mod collab;
pub(crate) mod color_management;
#[cfg(not(target_os = "android"))]
pub(crate) mod dev_shot;
pub(crate) mod document;
pub(crate) mod files;
pub(crate) mod frame_stats;
pub(crate) mod import;
pub(crate) mod init;
pub(crate) mod input;
pub(crate) mod jobs;
pub(crate) mod layout;
pub(crate) mod motion;
pub(crate) mod painter;
pub(crate) mod playback;
pub(crate) mod pressure_calibration;
pub(crate) mod rig;
pub(crate) mod settings;
pub(crate) mod shader_ops;
pub(crate) mod state;
pub(crate) mod stroke_ops;
pub(crate) mod tabs;
pub(crate) mod timelapse;
pub(crate) mod tools;
#[cfg(test)]
mod undo_tests;
pub(crate) mod vector_render;
pub(crate) mod view;

pub use painter::PainterApp;

/// Keep glibc from sitting on freed memory. Each thread gets its own arena
/// and a worker thread's arena only gives back its free top once it passes
/// the trim threshold, which glibc raises to 64 MB after a big buffer is
/// freed (and `malloc_trim` leaves those tops alone): 35 arenas idled at
/// 15-20 MB each, 0 used. So: 4 arenas, a fixed 2 MB threshold, and big
/// buffers up to 32 MB still from the heap as the raised threshold had them
/// (brush benches unchanged).
pub(crate) fn tune_malloc() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        const M_TRIM_THRESHOLD: i32 = -1;
        const M_MMAP_THRESHOLD: i32 = -3;
        const M_ARENA_MAX: i32 = -8;
        // SAFETY: called before any other thread exists; only tunes malloc.
        unsafe {
            mallopt(M_ARENA_MAX, 4);
            mallopt(M_TRIM_THRESHOLD, 2 << 20);
            mallopt(M_MMAP_THRESHOLD, 32 << 20);
        }
    }
    // Android's malloc gives freed memory back at once: a stroke's buffers,
    // made anew each sample, came back as fresh pages every time (eleven
    // times the page faults; painting slower). Kept a second instead.
    #[cfg(target_os = "android")]
    {
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        const M_DECAY_TIME: i32 = -100;
        // SAFETY: only tunes malloc.
        unsafe {
            mallopt(M_DECAY_TIME, 1);
        }
    }
}

/// Give the system back memory that was freed but kept: glibc holds on to
/// it (freed big buffers, each thread's arena), so a burst of work (presets
/// read on every core, a whole canvas uploaded) would stay in the app's
/// footprint for good.
pub(crate) fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: malloc_trim only returns free pages to the system.
        unsafe { malloc_trim(0) };
    }
}
