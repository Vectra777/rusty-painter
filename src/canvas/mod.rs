//! The document model: tiled layers ([`storage`]), blending and
//! compositing, undo history, and the pixel algorithms tools run on it
//! (fill, filters, gradient, liquify, inpaint, palette, text).
pub mod blend;
pub mod blend_modes;
pub mod color;
pub mod fill;
pub mod filters;
pub mod geometry;
pub mod gradient;
pub mod history;
pub mod inpaint;
pub mod layer_style;
pub mod liquify;
pub mod palette;
pub mod storage;
pub mod text;
pub mod vector;

pub use storage::Canvas;
