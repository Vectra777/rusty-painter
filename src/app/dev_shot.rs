//! Screenshots for checking the layout at a given window size, without
//! clicking around: `RP_SIZE=873x393 RP_SHOT=out.png RP_OPEN=brush,layers`
//! opens those panels (and `palette`, the Palette window), saves the window after a few frames and quits.
//! `RUSTY_PAINTER_TOUCH=1` gives the touch layout (Android, iPad).
//! `RP_DEMO=dir` paints a small scene with a few presets, a stroke at a
//! time as a pen would, and saves the window every other frame into `dir`
//! (the README's demo GIF and screenshot are made from them).

use crate::PainterApp;
use eframe::egui;

/// The window size `RP_SIZE` asks for.
pub(crate) fn size() -> Option<[f32; 2]> {
    let v = std::env::var("RP_SIZE").ok()?;
    let (w, h) = v.split_once('x')?;
    Some([w.parse().ok()?, h.parse().ok()?])
}

pub(crate) fn tick(app: &mut PainterApp, ctx: &egui::Context) {
    if let Some(dir) = std::env::var_os("RP_DEMO") {
        return demo(app, ctx, std::path::Path::new(&dir));
    }
    let Some(path) = std::env::var_os("RP_SHOT") else {
        return;
    };
    static FRAMES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let frame = FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if frame < 4 {
        open_panels(app);
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

/// The panels `RP_OPEN` names opened, the others closed.
fn open_panels(app: &mut PainterApp) {
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
    app.workspace.palette.open = open.contains("palette");
}

/// One stroke of the demo: a preset, its colour, its size and path (in
/// fractions of the canvas height and size), with the pen's pressure.
struct DemoStroke {
    preset: &'static str,
    color: [u8; 3],
    diameter: f32,
    path: fn(f32) -> (f32, f32, f32),
}

/// The hill's top at `x`.
fn hill(x: f32) -> f32 {
    0.62 - 0.12 * (x * 3.1).sin()
}

/// 0 → 1 → 0, `n` times as `t` goes from 0 to 1.
fn zigzag(t: f32, n: f32) -> f32 {
    1.0 - (2.0 * (t * n).fract() - 1.0).abs()
}

const DEMO: &[DemoStroke] = &[
    // The sky: a wash, back and forth down to the hill.
    DemoStroke {
        preset: "Watercolour Wash",
        color: [110, 160, 215],
        diameter: 0.2,
        path: |t| (0.05 + 0.9 * zigzag(t, 3.0), 0.05 + 0.5 * t, 1.0),
    },
    // The sun, filled from its rim in.
    DemoStroke {
        preset: "Hard Round",
        color: [245, 165, 45],
        diameter: 0.05,
        path: |t| {
            let (a, r) = (t * 25.0, 0.08 * (1.0 - t));
            (0.75 + r * a.cos(), 0.2 + r * a.sin(), 1.0)
        },
    },
    // The hill, filled up and down.
    DemoStroke {
        preset: "Wet Round",
        color: [70, 135, 75],
        diameter: 0.1,
        path: |t| {
            let x = t;
            (x, hill(x) + (1.06 - hill(x)) * zigzag(t, 12.0), 1.0)
        },
    },
    // Leaves along its top.
    DemoStroke {
        preset: "Foliage",
        color: [35, 100, 60],
        diameter: 0.06,
        path: |t| (0.03 + 0.94 * t, hill(0.03 + 0.94 * t) + 0.01, 1.0),
    },
    // Flowers in the grass.
    DemoStroke {
        preset: "Flowers",
        color: [235, 110, 150],
        diameter: 0.05,
        path: |t| (0.08 + 0.84 * t, 0.85 + 0.05 * (t * 9.0).sin(), 1.0),
    },
    // A few sparkles in the sky.
    DemoStroke {
        preset: "Stamp Sparkles",
        color: [255, 245, 200],
        diameter: 0.05,
        path: |t| (0.08 + 0.5 * t, 0.15 + 0.08 * (t * 7.0).sin(), 1.0),
    },
];

/// Pen samples a stroke takes, and how many go in each frame.
const DEMO_SAMPLES: usize = 150;
const DEMO_PER_FRAME: usize = 5;

fn demo(app: &mut PainterApp, ctx: &egui::Context, dir: &std::path::Path) {
    static FRAMES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static SAVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let frame = FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if frame < 4 {
        open_panels(app);
    }
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    // A few frames to settle (the canvas fitted to the window), then each
    // stroke over DEMO_SAMPLES / DEMO_PER_FRAME frames.
    let step = frame.saturating_sub(20) * DEMO_PER_FRAME;
    let (index, sample) = (step / DEMO_SAMPLES, step % DEMO_SAMPLES);
    if frame >= 20
        && let Some(stroke) = DEMO.get(index)
    {
        if sample == 0 {
            if let Some(i) = app
                .brush_state
                .presets
                .iter()
                .position(|p| p.name == stroke.preset)
            {
                app.apply_preset(i);
            }
            let b = &mut app.brush_state.brush;
            let [r, g, b2] = stroke.color;
            b.brush_options.color = egui::Color32::from_rgb(r, g, b2);
            b.brush_options.diameter = stroke.diameter * h;
            b.is_changed = true;
        }
        for k in sample..sample + DEMO_PER_FRAME {
            let (x, y, pressure) = (stroke.path)(k as f32 / (DEMO_SAMPLES - 1) as f32);
            let pos = egui::vec2(x * w, y * h);
            if k == 0 {
                app.start_stroke_with_pressure(pos, pressure);
            } else {
                app.add_stroke_point(pos, pressure);
            }
        }
        if sample + DEMO_PER_FRAME >= DEMO_SAMPLES {
            app.finish_stroke();
        }
    }
    let end = 20 + DEMO.len() * DEMO_SAMPLES / DEMO_PER_FRAME + 30;
    if frame >= 20 && frame.is_multiple_of(2) && frame <= end {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
    }
    for event in ctx.input(|i| i.raw.events.clone()) {
        if let egui::Event::Screenshot { image, .. } = event {
            let n = SAVED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let [iw, ih] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            let path = dir.join(format!("{n:04}.png"));
            if let Err(err) =
                image::save_buffer(path, &bytes, iw as u32, ih as u32, image::ColorType::Rgba8)
            {
                log::error!("demo frame: {err}");
            }
        }
    }
    if frame > end + 10 {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    ctx.request_repaint();
}
