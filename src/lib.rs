//! Rusty Painter: a tiled, multithreaded painting app on egui/wgpu.
//! The desktop and iOS binary calls [`run`]; Android enters at
//! `android_main`.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;
/// The system's file pickers, sharing and shared storage, on Android and iOS.
#[cfg(target_os = "android")]
use android as platform;
#[cfg(target_os = "ios")]
use ios as platform;
mod app;
#[cfg(feature = "bench")]
#[doc(hidden)]
pub mod bench_api;
pub mod brush_engine;
pub mod canvas;
#[cfg(test)]
mod fuzz;
pub(crate) mod project;
pub mod selection;
#[cfg(not(target_os = "android"))]
mod startup;
mod tablet;
mod ui;

pub use app::PainterApp;
pub use app::document::{BackgroundChoice, CanvasUnit, ColorModel, NewCanvasSettings, Orientation};

#[cfg(target_os = "android")]
use winit::platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp};

/// The window title (desktop) and app name.
pub(crate) const APP_NAME: &str = "Rusty Painter";

/// Launch the app (the binary's `main`): a window on the desktop, the whole
/// screen on iOS.
#[cfg(not(target_os = "android"))]
pub fn run() -> eframe::Result<()> {
    startup::init_logging();
    log::info!(
        "Starting {APP_NAME} {} on {} ({})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let viewport = if cfg!(target_os = "ios") {
        eframe::egui::ViewportBuilder::default().with_fullscreen(true)
    } else {
        eframe::egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([480.0, 360.0])
    };
    let options = eframe::NativeOptions {
        viewport,
        // The canvas is drawn by a custom wgpu pipeline (GPU mipmaps).
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    let result = eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            ui::theme::apply_global_style(&cc.egui_ctx);
            Ok(Box::new(PainterApp::new(cc)))
        }),
    );
    if let Err(err) = &result {
        log::error!("The app couldn't start or its event loop failed: {err}");
    }
    result
}

/// Android's private storage, set at start-up (the data folder there).
pub(crate) static ANDROID_DATA: std::sync::OnceLock<std::path::PathBuf> =
    std::sync::OnceLock::new();

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

    if let Some(dir) = app.internal_data_path() {
        let _ = ANDROID_DATA.set(dir);
    }
    let options = android_native_options(app);
    let _ = eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            ui::theme::apply_global_style(&cc.egui_ctx);
            Ok(Box::new(PainterApp::new(cc)))
        }),
    );
}
