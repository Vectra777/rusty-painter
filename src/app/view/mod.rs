//! Showing the canvas: GPU atlases of composited tiles ([`gpu_canvas`]),
//! which tiles to re-composite and upload each frame ([`render`]), and the
//! canvas <-> screen mapping (zoom, pan, rotation, flip; [`viewport`]).
//! Shader layers showing live are composited on the GPU ([`shader_gpu`]).
//! Viewing aids over it: the grid, guide lines and snapping ([`aids`]).
pub(crate) mod aids;
pub(crate) mod brush_cursor;
pub(crate) mod gpu_canvas;
pub(crate) mod grid;
pub(crate) mod guide_lines;
pub(crate) mod render;
pub(crate) mod shader_gpu;
pub(crate) mod viewport;
