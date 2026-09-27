//! Canvas storage, compositing, and history helpers.
pub mod blend;
pub mod blend_modes;
pub mod fill;
pub mod gradient;
pub mod history;
pub mod inpaint;
pub mod liquify;
pub mod palette;
pub mod storage;

pub use storage::Canvas;
