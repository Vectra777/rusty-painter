mod app;
mod brush_engine;
mod canvas;
mod gpu_painter;
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
        PaintBackend::Gpu => {
            println!("Launching GPU painting backend (wgpu)");
            if let Err(err) = gpu_painter::run() {
                eprintln!("Failed to start GPU backend: {err}");
            }
            Ok(())
        }
        PaintBackend::Cpu => {
            let options = eframe::NativeOptions {
                viewport: eframe::egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
                ..Default::default()
            };
            eframe::run_native(
                "Rust Dab Painter",
                options,
                Box::new(|cc| Ok(Box::new(PainterApp::new(cc)))),
            )
        }
    }
}
