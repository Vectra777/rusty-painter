//! Timings for the heavier features on a 4000 px canvas:
//! `cargo test --release --lib perf:: -- --ignored --nocapture --test-threads 1`
use super::*;
use crate::app::tools::Tool;
use crate::selection::SelectionType;
use crate::selection::transform::TransformInfo;
use eframe::egui::Vec2;
use std::time::Instant;

const N: usize = 4000;

/// Layer 1 fully painted with a smooth gradient plus some lines.
fn big_app() -> PainterApp {
    let canvas = Canvas::new(N, N, Color32::WHITE, TILE_SIZE);
    let tiles = N.div_ceil(TILE_SIZE);
    for ty in 0..tiles {
        for tx in 0..tiles {
            let t: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
                .map(|i| {
                    let (x, y) = (
                        tx * TILE_SIZE + i % TILE_SIZE,
                        ty * TILE_SIZE + i / TILE_SIZE,
                    );
                    if x % 250 < 3 || y % 250 < 3 {
                        Color32::BLACK
                    } else {
                        Color32::from_rgb((x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8)
                    }
                })
                .collect();
            canvas.set_layer_tile_data(1, tx as i32, ty as i32, t);
        }
    }
    let mut app = tests::test_app_pub(canvas);
    app.canvas_mut().active_layer_idx = 1;
    // Every core, as the app has (the test app has one thread).
    app.workspace.pool = std::sync::Arc::new(rayon::ThreadPoolBuilder::new().build().unwrap());
    app
}

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    eprintln!("{label:<40} {:>10.1?}", t.elapsed());
    r
}

/// A smudge-tool or mixing-brush stroke across the canvas, painted on
/// the stroke worker and waited for.
#[test]
#[ignore = "timing"]
fn smudge() {
    let path: Vec<(Vec2, f32)> = (0..240)
        .map(|i| {
            let t = i as f32 / 239.0;
            let p = Vec2::new(800.0 + 2400.0 * t, 2000.0 + 600.0 * (t * 9.0).sin());
            (p, 0.3 + 0.7 * t)
        })
        .collect();
    for diameter in [40.0, 80.0, 200.0] {
        for (what, smudge, mixing) in [
            ("smudge", true, false),
            ("blur", false, false),
            ("mixing brush", false, true),
        ] {
            let mut app = big_app();
            app.brush_state.brush.brush_options.diameter = diameter;
            if mixing {
                app.brush_state.brush.mixing = Some(Default::default());
            }
            let started = Instant::now();
            if mixing {
                app.set_brush_tool(false);
                app.start_stroke_with_pressure(path[0].0, path[0].1);
                for &(p, pressure) in &path[1..] {
                    app.add_stroke_point(p, pressure);
                }
                app.finish_stroke();
            } else {
                app.set_blend_tool(smudge);
                app.blend_press(path[0].0, path[0].1);
                for &(p, pressure) in &path[1..] {
                    app.blend_drag(p, pressure);
                }
                app.blend_release();
            }
            app.settle_strokes();
            eprintln!(
                "{:<40} {:>10.1?}",
                format!("{what}: {diameter} px, 240 samples"),
                started.elapsed()
            );
        }
    }
    // Krita's colour smudge (imported presets), the default one and
    // the ones in the bundles at hand.
    let stroke_with = |label: &str, brush: crate::brush_engine::brush::Brush| {
        let mut app = big_app();
        app.brush_state.brush = brush;
        app.set_brush_tool(false);
        let started = Instant::now();
        app.start_stroke_with_pressure(path[0].0, path[0].1);
        for &(p, pressure) in &path[1..] {
            app.add_stroke_point(p, pressure);
        }
        app.finish_stroke();
        app.settle_strokes();
        eprintln!("{label:<60} {:>10.1?}", started.elapsed());
    };
    for diameter in [80.0, 200.0] {
        let mut brush = big_app().brush_state.brush.clone();
        brush.brush_options.diameter = diameter;
        brush.mixing = Some(crate::brush_engine::brush_options::Mixing {
            krita: Some(Default::default()),
            ..Default::default()
        });
        stroke_with(&format!("krita smudge: {diameter} px"), brush);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for bundle in [
        "Peaches_Painting_Brushes.bundle",
        "Rakurri_Brush_Set_V2.0.bundle",
    ] {
        let Ok(bytes) = std::fs::read(format!("{home}/Downloads/{bundle}")) else {
            continue;
        };
        let Ok(imported) = crate::brush_engine::import::import(bundle, &bytes) else {
            continue;
        };
        let smudges = imported
            .presets
            .into_iter()
            .filter(|p| p.brush.mixing.is_some_and(|m| m.krita.is_some()));
        for preset in smudges.take(4) {
            stroke_with(
                &format!(
                    "{} ({} px)",
                    preset.name, preset.brush.brush_options.diameter
                ),
                preset.brush,
            );
        }
    }
}

#[test]
#[ignore = "timing"]
fn export() {
    let app = big_app();
    let img = time("export: flatten", || app.canvas.flatten());
    {
        let mut reference = eframe::egui::ColorImage::new([N, N], Color32::TRANSPARENT);
        app.canvas
            .write_region_to_color_image(0, 0, N, N, &mut reference, 1);
        assert!(reference.pixels == img.pixels, "parallel flatten matches");
    }
    let rgba = time("export: to rgba", || {
        crate::project::export::to_rgba_image(img.clone()).unwrap()
    });
    time("export: encode png", || {
        let mut out = Vec::new();
        rgba.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        eprintln!("  png size {} KB", out.len() / 1024);
    });
    // A noisy, photo-like picture compresses much harder.
    let noisy = image::RgbaImage::from_fn(N as u32, N as u32, |x, y| {
        let n = (x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) >> 27;
        image::Rgba([
            (x / 16) as u8 ^ n as u8,
            (y / 16) as u8,
            ((x + y) / 32) as u8,
            255,
        ])
    });
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    for (label, c, f) in [
        (
            "png default",
            CompressionType::Default,
            FilterType::Adaptive,
        ),
        ("png fast", CompressionType::Fast, FilterType::Adaptive),
        ("png best", CompressionType::Best, FilterType::Adaptive),
    ] {
        time(&format!("export: noisy {label}"), || {
            let mut out = Vec::new();
            PngEncoder::new_with_quality(&mut out, c, f)
                .write_image(&noisy, N as u32, N as u32, image::ExtendedColorType::Rgba8)
                .unwrap();
            eprintln!("  {label}: {} KB", out.len() / 1024);
        });
    }
    for f in [
        crate::project::export::ExportFormat::Jpeg,
        crate::project::export::ExportFormat::Tiff,
    ] {
        time(&format!("export: {}", f.label()), || {
            let r = crate::project::export::encode_color_image(img.clone(), f);
            eprintln!(
                "  {:?}",
                r.as_ref().map(|b| b.len() / 1024).map_err(|e| e.clone())
            );
        });
    }
    time("export: whole (encode_color_image png)", || {
        crate::project::export::encode_color_image(img, crate::project::export::ExportFormat::Png)
            .unwrap()
    });
}

#[test]
#[ignore = "timing"]
fn transform() {
    let mut app = big_app();
    app.active_tool = Tool::Transform(TransformInfo::default());
    time("transform: float whole layer", || {
        transform_press_at(&mut app)
    });
    for (label, dragging, rot) in [
        ("transform: 1st preview (bounds scan)", false, 0.1),
        ("transform: preview rotate, full quality", false, 0.17),
        ("transform: preview rotate, while dragging", true, 0.2),
    ] {
        time(label, || {
            if let Tool::Transform(ref mut i) = app.active_tool {
                i.rotation = rot;
                i.start_pos = dragging.then_some(Vec2::ZERO);
            }
            app.layer_state.transform_preview_pending = true;
            crate::app::tools::transform::flush_transform_preview(&mut app);
        });
    }
    time("transform: preview (move 7px)", || {
        if let Tool::Transform(ref mut i) = app.active_tool {
            i.rotation = 0.0;
            i.offset = Vec2::new(7.0, 3.0);
        }
        app.layer_state.transform_preview_pending = true;
        crate::app::tools::transform::flush_transform_preview(&mut app);
    });
    time("transform: commit", || {
        crate::app::tools::transform::commit_floating_layer(&mut app)
    });
    // A selection float.
    app.selection_manager
        .start_selection(Vec2::new(500.0, 500.0), SelectionType::Rectangle);
    app.selection_manager
        .update_selection(Vec2::new(1500.0, 1500.0));
    app.selection_manager.end_selection();
    time("transform: float 1000px selection", || {
        transform_press_at(&mut app)
    });
    time("transform: cancel", || {
        crate::app::tools::transform::cancel_floating_layer(&mut app)
    });
}

fn transform_press_at(app: &mut PainterApp) {
    crate::app::tools::transform::transform_press(app, Vec2::new(1000.0, 1000.0));
    crate::app::tools::transform::transform_release(app);
}

#[test]
#[ignore = "timing"]
fn liquify() {
    let mut app = big_app();
    app.active_tool = Tool::Liquify;
    app.workspace.liquify.radius = 150.0;
    time("liquify: begin + 1st dab", || {
        app.liquify_press(Vec2::new(2000.0, 2000.0))
    });
    // Each move as a frame: the field, then the layer drawn.
    time("liquify: 300 px drag", || {
        for i in 1..=30 {
            app.liquify_drag(Vec2::new(2000.0 + i as f32 * 10.0, 2000.0));
            app.liquify_flush();
        }
    });
    app.liquify_release();
    time("liquify: commit", || app.liquify_commit());
    use crate::canvas::liquify::LiquifyMode;
    for mode in [
        LiquifyMode::TwirlCw,
        LiquifyMode::Pinch,
        LiquifyMode::Bloat,
        LiquifyMode::Smooth,
    ] {
        app.workspace.liquify.mode = mode;
        app.liquify_press(Vec2::new(2000.0, 2000.0));
        time(&format!("liquify: {mode:?} hold, 30 frames"), || {
            for _ in 0..30 {
                app.liquify_hold(1.0 / 60.0);
                app.liquify_flush();
            }
        });
        time(&format!("liquify: {mode:?} 300 px drag"), || {
            for i in 1..=30 {
                app.liquify_drag(Vec2::new(2000.0 + i as f32 * 10.0, 2000.0));
                app.liquify_flush();
            }
        });
        app.liquify_release();
        app.liquify_commit();
    }
}

#[test]
#[ignore = "timing"]
fn palette() {
    let mut app = big_app();
    app.workspace.palette.count = 16;
    time("palette: extract 16 (all visible)", || {
        app.extract_palette()
    });
    app.workspace.palette.from_layer = true;
    time("palette: extract 16 (layer)", || app.extract_palette());
    let pal = app.workspace.palette.extracted.clone();
    time("palette: recolour layer", || app.recolor_layer(&pal));
    app.workspace.palette.dither = true;
    time("palette: recolour layer (dither)", || {
        app.recolor_layer(&pal)
    });
}

#[test]
#[ignore = "timing"]
fn import() {
    let mut app = big_app();
    let img =
        image::RgbaImage::from_fn(3000, 2000, |x, y| image::Rgba([x as u8, y as u8, 90, 255]));
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    time("import: 3000x2000 png (decode+place)", || {
        app.import_image_bytes("p", &png).unwrap()
    });
    let big = image::RgbaImage::from_pixel(6000, 6000, image::Rgba([10, 20, 30, 255]));
    time("import: 6000x6000 raw (resize+place)", || {
        app.import_rgba("big", big)
    });
}

#[test]
#[ignore = "timing"]
fn selection_and_enclose() {
    let mut app = big_app();
    app.selection_manager.canvas_size = [N, N];
    app.selection_manager.brush_radius = 100.0;
    time("selection brush: 60 px stroke x 40", || {
        app.selection_manager
            .start_selection(Vec2::new(500.0, 500.0), SelectionType::Brush);
        for i in 0..40 {
            app.selection_manager
                .update_selection(Vec2::new(500.0 + i as f32 * 60.0, 500.0 + i as f32 * 30.0));
        }
        app.selection_manager.end_selection();
    });
    time("selection: invert", || app.selection_manager.invert());
    time("selection: outline", || {
        if let Some(crate::selection::SelectionShape::Mask(m)) =
            &app.selection_manager.current_shape
        {
            m.outline().len()
        } else {
            0
        }
    });
    app.selection_manager.clear_selection();
    let lasso: Vec<Vec2> = (0..200)
        .map(|i| {
            let a = i as f32 / 200.0 * std::f32::consts::TAU;
            Vec2::new(2000.0 + a.cos() * 1500.0, 2000.0 + a.sin() * 1500.0)
        })
        .collect();
    app.workspace.fill.mode = crate::app::tools::fill::FillMode::Enclose;
    app.workspace.fill.path = lasso.clone();
    time("enclose fill: 3000 px lasso", || app.fill_release());
}
#[test]
#[ignore = "timing"]
fn every_filter() {
    use crate::canvas::filters::Filter;
    let mut app = big_app();
    // As the app runs them: the UI thread queues the filter, the
    // stroke worker runs it.
    app.workspace.jobs.defer = true;
    for f in Filter::MENU.iter().flat_map(|g| g.iter()) {
        let started = std::time::Instant::now();
        app.filter_open(*f);
        app.filter_commit();
        let ui = started.elapsed();
        app.run_jobs();
        app.release_canvas();
        eprintln!(
            "filter: {:<28} UI thread {:>7.1?}   done {:>7.1?}",
            f.name(),
            ui,
            started.elapsed()
        );
        app.apply_history_now(false);
    }
}

#[test]
#[ignore = "timing"]
fn filter_stages() {
    // Where the time of applying a cheap filter to a whole layer goes.
    use crate::canvas::filters::Filter;
    let mut app = big_app();
    let pool = std::sync::Arc::clone(&app.workspace.pool);
    let canvas = &app.canvas;
    let bounds = [0, 0, N as i32, N as i32];
    let original = time("filter stage: capture region", || {
        pool.install(|| canvas.capture_region(1, bounds))
    });
    let src = time("filter stage: region to pixels", || {
        pool.install(|| original.pixels(canvas.tile_size()))
    });
    let out = time("filter stage: apply (invert)", || {
        pool.install(|| Filter::Invert.apply(&src, N, N, (0, 0)))
    });
    time("filter stage: write back", || {
        pool.install(|| canvas.replace_region(1, &original, &out, None))
    });
    let tiles = time("filter stage: undo snapshots", || {
        pool.install(|| canvas.region_snapshots(1, &original))
    });
    time("filter stage: push undo", || {
        app.push_undo(crate::canvas::history::UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: None,
        })
    });
}

#[test]
#[ignore = "timing"]
fn layer_kinds_and_styles() {
    use crate::canvas::filters::Filter;
    use crate::canvas::layer_style::{Border, LayerFill, LayerStyle};
    let mut app = big_app();
    time("composite: two paint layers", || app.canvas.flatten());
    for f in Filter::ADJUSTMENTS {
        app.canvas_mut().active_layer_idx = 1;
        app.add_adjustment_layer(f);
        let idx = app.canvas.active_layer_idx;
        time(&format!("composite: + {} layer", f.name()), || {
            app.canvas.flatten()
        });
        app.remove_layer(idx);
    }
    for (label, fill) in [
        ("colour", LayerFill::Colour([30, 90, 200])),
        (
            "gradient",
            LayerFill::Gradient {
                colours: Default::default(),
                shape: crate::canvas::gradient::GradientShape::Linear,
                start: [0.0, 0.0],
                end: [N as f32, N as f32],
            },
        ),
    ] {
        app.canvas_mut().active_layer_idx = 1;
        app.add_fill_layer(fill);
        let idx = app.canvas.active_layer_idx;
        time(&format!("composite: + {label} fill layer"), || {
            app.canvas.flatten()
        });
        app.remove_layer(idx);
    }
    for width in [4.0, 16.0, 40.0] {
        app.set_layer_style(
            1,
            LayerStyle {
                fill: None,
                border: Some(Border {
                    width,
                    ..Border::default()
                }),
                impasto: None,
                lightness_map: false,
            },
        );
        time(
            &format!("composite: {width} px border on the layer"),
            || app.canvas.flatten(),
        );
    }
    app.set_layer_style(1, LayerStyle::default());

    app.add_vector_layer();
    let idx = app.canvas.active_layer_idx;
    let lines: Vec<Vec<Vec2>> = (0..500)
        .map(|i| {
            let y = 100.0 + i as f32 * 7.5;
            (0..60)
                .map(|k| Vec2::new(100.0 + k as f32 * 60.0, y + (k as f32 * 0.7).sin() * 40.0))
                .collect()
        })
        .collect();
    time("vector: add 500 lines of 60 points", || {
        for l in &lines {
            app.add_vector_line(idx, l);
        }
    });
    time("composite: + 500-line vector layer", || {
        app.canvas.flatten()
    });
    time("vector: rasterise the layer", || {
        app.rasterise_vector_layer(idx)
    });
}

#[test]
#[ignore = "profiling: a small tree composite to run under callgrind"]
fn tree_composite_profile() {
    let canvas = Canvas::new(1024, 1024, Color32::WHITE, TILE_SIZE);
    for ty in 0..16 {
        for tx in 0..16 {
            let t: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
                .map(|i| Color32::from_rgb((i % 251) as u8, (tx * 16) as u8, (ty * 16) as u8))
                .collect();
            canvas.set_layer_tile_data(1, tx, ty, t);
        }
    }
    let mut app = tests::test_app_pub(canvas);
    app.canvas_mut().active_layer_idx = 1;
    app.add_folder();
    for _ in 0..3 {
        std::hint::black_box(app.canvas.flatten());
    }
}

#[test]
#[ignore = "timing"]
fn every_export_format() {
    use crate::project::export::{ExportFormat, encode_color_image};
    let app = big_app();
    let img = app.canvas.flatten();
    for f in ExportFormat::ALL {
        time(&format!("export: {}", f.label()), || {
            let size = match f {
                ExportFormat::Psd => psd::encode_psd(&psd::PsdDocument::from_canvas(&app.canvas))
                    .unwrap()
                    .len(),
                ExportFormat::Svg => svg::document_svg(&app.canvas).unwrap().len(),
                _ => encode_color_image(img.clone(), f).unwrap().len(),
            };
            eprintln!("  {} KB", size / 1024);
        });
    }
}

#[test]
#[ignore = "timing"]
fn open_other_apps_documents() {
    // A 4000 px PSD of the test picture; set `RP_OPEN` to other files
    // (.kra, .clip) to time those too.
    let app = big_app();
    let bytes = psd::encode_psd(&psd::PsdDocument::from_canvas(&app.canvas)).unwrap();
    time("open: 4000 px psd (decode)", || {
        psd::decode_psd(&bytes).unwrap()
    });
    let doc = psd::decode_psd(&bytes).unwrap();
    time("open: 4000 px psd (to layers)", || {
        doc.into_canvas().unwrap()
    });
    for path in std::env::var("RP_OPEN").unwrap_or_default().split(':') {
        let path = std::path::Path::new(path);
        let Some(decode) = path
            .extension()
            .and_then(|e| foreign_decoder(&e.to_string_lossy().to_ascii_lowercase()))
        else {
            continue;
        };
        let bytes = fs::read(path).unwrap();
        let doc = time(&format!("open: {} (decode)", path.display()), || {
            decode(&bytes).unwrap()
        });
        time(&format!("open: {} (to layers)", path.display()), || {
            doc.into_canvas().unwrap()
        });
    }
}
