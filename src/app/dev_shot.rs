//! Screenshots for checking the layout at a given window size, without
//! clicking around: `RP_SIZE=873x393 RP_SHOT=out.png RP_OPEN=brush,layers`
//! opens those panels, saves the window after a few frames and quits.
//! `RUSTY_PAINTER_TOUCH=1` gives the touch layout (Android, iPad).

use crate::PainterApp;
use eframe::egui;

/// The window size `RP_SIZE` asks for.
pub(crate) fn size() -> Option<[f32; 2]> {
    let v = std::env::var("RP_SIZE").ok()?;
    let (w, h) = v.split_once('x')?;
    Some([w.parse().ok()?, h.parse().ok()?])
}

pub(crate) fn tick(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(path) = std::env::var_os("RP_SHOT") else {
        return;
    };
    static FRAMES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let frame = FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if frame < 4 {
        let open = std::env::var("RP_OPEN").unwrap_or_default();
        let ws = &mut app.workspace;
        ws.touch_mode = std::env::var_os("RUSTY_PAINTER_TOUCH").is_some();
        ws.library.open = open.contains("library");
        ws.show_left_panel = open.contains("brush");
        ws.show_color = open.contains("colour");
        ws.show_layers = open.contains("layers");
        app.brush_state.show_presets = open.contains("presets");
        app.modal_state.show_general_settings = open.contains("settings");
        app.modal_state.menu_sheet_open = open.contains("sheet");
        app.modal_state.show_new_canvas_modal = open.contains("newcanvas");
        app.export_state.show_modal = open.contains("export");
    }
    if frame == 40 {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
    }
    for event in ctx.input(|i| i.raw.events.clone()) {
        if let egui::Event::Screenshot { image, .. } = event {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Err(err) =
                image::save_buffer(&path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
            {
                log::error!("dev shot: {err}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    ctx.request_repaint();
}
