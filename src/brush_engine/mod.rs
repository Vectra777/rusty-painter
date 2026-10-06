//! Brush rendering logic and stroke handling.
pub mod bristle;
pub mod brush;
pub mod brush_options;
pub(crate) mod dab;
pub mod dual;
pub mod dynamics;
#[cfg(test)]
mod dynamics_tests;
pub mod hardness;
pub mod hatching;
pub mod import;
pub(crate) mod masks;
pub mod preset_file;
pub mod preview;
pub mod sketch;
#[cfg(test)]
mod smoothness_tests;
pub mod stabilizer;
pub mod stroke;
pub mod stroke_worker;
pub mod symmetry;
pub mod texture;
pub mod tip;
pub mod wet_edge;
