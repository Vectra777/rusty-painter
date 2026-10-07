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
//! - [`color_management`]: the monitor's profile, proofing, assigning and
//!   converting the document's profile.
//! - [`animation`]: frames, playback, drawings and exporting animations.
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
pub(crate) mod color_management;
pub(crate) mod document;
pub(crate) mod files;
pub(crate) mod frame_stats;
pub(crate) mod import;
pub(crate) mod init;
pub(crate) mod input;
pub(crate) mod jobs;
pub(crate) mod layout;
pub(crate) mod painter;
pub(crate) mod pressure_calibration;
pub(crate) mod settings;
pub(crate) mod shader_ops;
pub(crate) mod state;
pub(crate) mod stroke_ops;
pub(crate) mod timelapse;
pub(crate) mod tools;
#[cfg(test)]
mod undo_tests;
pub(crate) mod view;

pub use painter::PainterApp;
