mod app;
#[cfg(feature = "bench")]
#[doc(hidden)]
pub mod bench_api;
pub mod brush_engine;
pub mod canvas;
pub(crate) mod project;
pub mod selection;
mod styling;
mod tablet;
mod ui;
mod utils;

pub use app::PainterApp;
pub use app::state::{BackgroundChoice, CanvasUnit, ColorModel, NewCanvasSettings, Orientation};

#[cfg(target_os = "android")]
use winit::platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp};

/// The window title (desktop) and app name.
const APP_NAME: &str = "Rust Dab Painter";

/// Launch the desktop app (the binary's `main`).
#[cfg(not(target_os = "android"))]
pub fn run() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([480.0, 360.0]),
        // The canvas is drawn by a custom wgpu pipeline (GPU mipmaps).
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            styling::apply_global_style(&cc.egui_ctx);
            Ok(Box::new(PainterApp::new(cc)))
        }),
    )
}

#[cfg(target_os = "android")]
fn android_native_options(app: AndroidApp) -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_fullscreen(true),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    options.event_loop_builder = Some(Box::new(move |builder| {
        builder.with_android_app(app);
    }));
    options
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    let options = android_native_options(app);
    let _ = eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            styling::apply_global_style(&cc.egui_ctx);
            Ok(Box::new(PainterApp::new(cc)))
        }),
    );
}
