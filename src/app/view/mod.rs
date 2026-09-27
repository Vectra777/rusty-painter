//! Showing the canvas: GPU atlases of composited tiles ([`gpu_canvas`]),
//! which tiles to re-composite and upload each frame ([`render`]), and the
//! canvas <-> screen mapping (zoom, pan, rotation, flip; [`viewport`]).
pub(crate) mod gpu_canvas;
pub(crate) mod render;
pub(crate) mod viewport;
