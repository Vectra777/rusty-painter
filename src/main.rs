#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]


mod app;
mod brush_engine;
mod canvas;
mod styling;
mod ui;
mod utils;

pub use app::state::{
    BackgroundChoice, CanvasUnit, ColorDepth, ColorModel, NewCanvasSettings, Orientation,
};
pub use app::{PaintBackend, PainterApp, parse_backend_arg};

/// Launch the native egui application.
fn main() -> eframe::Result<()> {
    env_logger::init();

    match parse_backend_arg() {
        PaintBackend::Cpu => {
            let options = eframe::NativeOptions {
                viewport: eframe::egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
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
    }
}
