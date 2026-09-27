//! Brush rendering logic and stroke handling.
pub mod brush;
pub mod brush_options;
mod dab;
pub mod dynamics;
#[cfg(test)]
mod dynamics_tests;
pub mod hardness;
pub(crate) mod masks;
pub mod preview;
pub mod stabilizer;
pub mod stroke;
pub mod stroke_worker;
pub mod symmetry;
pub mod tip;
