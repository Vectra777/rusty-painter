#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The desktop binary: everything lives in the library crate.

fn main() -> eframe::Result<()> {
    rusty_painter::run()
}
