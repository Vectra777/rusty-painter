//! The application: one `PainterApp` owning the document, the tools and
//! the UI state, updated once a frame by egui.
//!
//! - [`painter`]: the `PainterApp` type and the frame loop (`update`).
//! - [`state`]: the app state groups `PainterApp` is made of.
//! - [`document`]: document settings, limits and tile types.
//! - [`canvas_ops`], [`stroke_ops`]: operations on the document (new canvas,
//!   layers, brush strokes through the stroke worker).
//! - [`input`]: pointer, pen, touch and keyboard input, routed to tools.
//! - [`tools`]: one module per tool (selection, fill, gradient, shapes...).
//! - [`view`]: drawing the canvas (GPU atlases) and the screen mapping.
//! - [`layout`], [`init`], [`import`], [`brush_io`]: docks, startup, files
//!   dropped or imported, brush tips on disk.
pub(crate) mod brush_io;
pub(crate) mod canvas_ops;
pub(crate) mod document;
pub(crate) mod import;
pub(crate) mod init;
pub(crate) mod input;
pub(crate) mod layout;
pub(crate) mod painter;
pub(crate) mod state;
pub(crate) mod stroke_ops;
pub(crate) mod tools;
pub(crate) mod view;

pub use painter::PainterApp;
