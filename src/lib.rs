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
    app::tune_malloc();
    startup::init_logging();
    let power_preference = app::settings::startup_power_preference();
    app::settings::keep_discrete_gpu_asleep(power_preference);
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
            .with_inner_size(app::dev_shot::size().unwrap_or([1440.0, 900.0]))
            .with_min_inner_size(app::dev_shot::size().unwrap_or([480.0, 360.0]))
            .with_icon(window_icon())
            // Wayland matches it to assets/linux/io.vectra.rustypainter.desktop's icon.
            .with_app_id("io.vectra.rustypainter")
    };
    let options = eframe::NativeOptions {
        viewport,
        // The canvas is drawn by a custom wgpu pipeline (GPU mipmaps).
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            power_preference,
            ..Default::default()
        },
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

/// The window and taskbar icon (the .exe's own is set in build.rs).
#[cfg(not(target_os = "android"))]
fn window_icon() -> eframe::egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/icon/icon-256.png"))
        .expect("the bundled icon is a PNG")
        .into_rgba8();
    eframe::egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
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
    app::tune_malloc();
    if let Err(err) = android::guard_back() {
        log::error!("Back: {err}");
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
    // The activity ended and everything is saved: end the process too, so
    // opening the app again starts it fresh (android-activity can't start a
    // second activity in the same process).
    std::process::exit(0);
}
