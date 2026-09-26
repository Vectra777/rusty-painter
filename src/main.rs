#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod brush_engine;
mod canvas;
mod project;
mod selection;
mod styling;
mod tablet;
mod ui;
mod utils;

pub use app::PainterApp;
pub use app::state::{BackgroundChoice, CanvasUnit, ColorModel, NewCanvasSettings, Orientation};

/// Launch the native egui application.
fn main() -> eframe::Result<()> {
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
        // The canvas is drawn by a custom wgpu pipeline (GPU mipmaps).
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Rust Dab Painter",
        options,
        Box::new(|cc| {
            styling::apply_global_style(&cc.egui_ctx);
            Ok(Box::new(PainterApp::new(cc)))
        }),
    )
}
