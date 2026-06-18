mod app;
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
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    let mut options = eframe::NativeOptions::default();
    options.event_loop_builder = Some(Box::new(move |builder| {
        builder.with_android_app(app);
    }));

    let _ = eframe::run_native(
        "Rusty Painter",
        options,
        Box::new(|cc| {
            styling::apply_global_style(&cc.egui_ctx);
            Ok(Box::new(PainterApp::new(cc)))
        }),
    );
}
