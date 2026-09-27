mod app;
#[cfg(feature = "bench")]
#[doc(hidden)]
pub mod bench_api;
pub mod brush_engine;
pub mod canvas;
pub(crate) mod project;
pub mod selection;
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod styling;
mod tablet;
mod ui;
mod utils;

pub use app::PainterApp;
pub use app::state::{BackgroundChoice, CanvasUnit, ColorModel, NewCanvasSettings, Orientation};

#[cfg(target_os = "android")]
use winit::platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp};

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
        "Rust Dab Painter",
        options,
        Box::new(|cc| {
            styling::apply_global_style(&cc.egui_ctx);
            Ok(Box::new(PainterApp::new(cc)))
        }),
    );
}
