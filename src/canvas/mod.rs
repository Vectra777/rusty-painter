//! The document model: tiled layers ([`storage`]), blending and
//! compositing, undo history, and the pixel algorithms tools run on it
//! (fill, gradient, liquify, inpaint, palette).
pub mod blend;
pub mod blend_modes;
pub mod color;
pub mod fill;
pub mod gradient;
pub mod history;
pub mod inpaint;
pub mod liquify;
pub mod palette;
pub mod storage;

pub use storage::Canvas;
