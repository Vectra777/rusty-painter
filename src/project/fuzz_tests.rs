use super::*;
use eframe::egui::Vec2;

/// A small document with one of most things: paint, a folder, a mask,
/// vector lines, a fill layer, an adjustment layer and undo steps.
fn rich_app() -> PainterApp {
    let canvas = Canvas::new(96, 64, Color32::WHITE, TILE_SIZE);
    let mut app = tests::test_app_pub(canvas);
    app.canvas_mut().active_layer_idx = 1;
    app.filter_open(crate::canvas::filters::Filter::Invert);
    app.add_folder();
    app.add_vector_layer();
    let idx = app.canvas.active_layer_idx;
    app.add_vector_line(idx, &[Vec2::new(5.0, 5.0), Vec2::new(80.0, 50.0)]);
    app.add_fill_layer(crate::canvas::layer_style::LayerFill::Colour([10, 200, 90]));
    app.add_adjustment_layer(crate::canvas::filters::Filter::Levels {
        black: 0.1,
        white: 0.9,
        gamma: 1.2,
    });
    app.canvas_mut().active_layer_idx = 1;
    app.add_mask_to_active();
    // Impasto paint, and a stroke of it in the history.
    app.brush_state.brush.impasto = Some(Default::default());
    app.start_stroke_with_pressure(Vec2::new(10.0, 10.0), 1.0);
    app.add_stroke_point(Vec2::new(60.0, 40.0), 1.0);
    app.finish_stroke();
    app.release_canvas();
    app.brush_state.brush.impasto = None;
    app
}

fn open(bytes: &[u8]) {
    let Ok(loaded) = decode_project(bytes) else {
        return;
    };
    let mut canvas = loaded.canvas;
    let mut history = loaded.history;
    canvas.flatten();
    // Every step back and forward again.
    let mut selection = crate::selection::SelectionManager::new();
    let mut tool = crate::app::tools::Tool::Brush;
    for _ in 0..64 {
        history.undo(&mut canvas, &mut selection, &mut tool);
    }
    canvas.flatten();
    for _ in 0..64 {
        history.redo(&mut canvas, &mut selection, &mut tool);
    }
    canvas.flatten();
}

/// Every value in `v`, depth first (for picking one to change).
fn count(v: &serde_json::Value) -> usize {
    1 + match v {
        serde_json::Value::Array(a) => a.iter().map(count).sum(),
        serde_json::Value::Object(o) => o.values().map(count).sum(),
        _ => 0,
    }
}

/// Replace value number `n` (depth first) with `with`.
fn replace(v: &mut serde_json::Value, n: &mut usize, with: &serde_json::Value) -> bool {
    if *n == 0 {
        *v = with.clone();
        return true;
    }
    *n -= 1;
    match v {
        serde_json::Value::Array(a) => a.iter_mut().any(|x| replace(x, n, with)),
        serde_json::Value::Object(o) => o.values_mut().any(|x| replace(x, n, with)),
        _ => false,
    }
}

#[test]
#[ignore = "fuzzing"]
fn fuzz_project_values() {
    // Well-formed files with nonsense in them: sizes, indices, ids and
    // counts at extremes, wrong types, missing parts.
    let bare = encode_project_data(&ProjectSnapshot::capture(&rich_app())).unwrap();
    let start = MAGIC.len() + 8;
    let len = u64::from_le_bytes(bare[MAGIC.len()..start].try_into().unwrap()) as usize;
    let manifest: serde_json::Value = serde_json::from_slice(&bare[start..start + len]).unwrap();
    let blobs = &bare[start + len..];
    let extremes: Vec<serde_json::Value> = serde_json::from_str(
        r#"[0, 1, -1, 2, 7, 64, 65, 4294967295, 18446744073709551615, -9223372036854775808,
            1e30, -1e30, 0.5, null, "", "x", [], {}, true, [0], [0, 0, 0, 0]]"#,
    )
    .unwrap();
    let total = count(&manifest);
    let rounds: usize = std::env::var("RP_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    // One value changed per input, walking every value with every
    // extreme first, then random pairs.
    let mut inputs = Vec::new();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for i in 0..rounds {
        let mut m = manifest.clone();
        for _ in 0..1 + i % 2 {
            let mut n = rnd(total);
            replace(&mut m, &mut n, &extremes[rnd(extremes.len())]);
        }
        let json = serde_json::to_vec(&m).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend((json.len() as u64).to_le_bytes());
        bytes.extend(json);
        bytes.extend(blobs);
        inputs.push(bytes);
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failures = Vec::new();
    let mut slowest = std::time::Duration::ZERO;
    for (i, input) in inputs.iter().enumerate() {
        let t = std::time::Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| open(input)));
        slowest = slowest.max(t.elapsed());
        if let Err(e) = r {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!("rp-fuzz-values-{i}.bin"));
            std::fs::write(&path, input).unwrap();
            failures.push(format!("{msg} ({})", path.display()));
        } else if t.elapsed() > std::time::Duration::from_secs(2) {
            failures.push(format!("round {i} took {:?}", t.elapsed()));
        }
    }
    std::panic::set_hook(hook);
    eprintln!(
        "fuzz project values: {} rounds, slowest {slowest:?}, {} failures",
        inputs.len(),
        failures.len()
    );
    failures.sort();
    failures.dedup_by(|a, b| a.split(" (").next() == b.split(" (").next());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "replays a saved fuzz input: RP_REPLAY=path"]
fn replay() {
    open(&std::fs::read(std::env::var("RP_REPLAY").unwrap()).unwrap());
}

#[test]
fn folders_inside_each_other_in_a_damaged_file_are_undone() {
    let mut app = rich_app();
    app.add_folder();
    let a = app.canvas.layers[app.canvas.active_layer_idx].id;
    app.add_folder();
    let bi = app.canvas.active_layer_idx;
    let b = app.canvas.layers[bi].id;
    let ai = app.canvas.layer_index_of(a).unwrap();
    app.canvas_mut().layers[ai].parent = Some(b);
    app.canvas_mut().layers[bi].parent = Some(a);
    let loaded =
        decode_project(&encode_project_data(&ProjectSnapshot::capture(&app)).unwrap()).unwrap();
    let parent_of = |id| {
        let c = &loaded.canvas;
        c.layers[c.layer_index_of(id).unwrap()].parent
    };
    // One of the two lets go; the other stays inside it.
    assert!(parent_of(a).is_none() != parent_of(b).is_none());
    let mut canvas = loaded.canvas;
    canvas.merge_visible().unwrap();
}

#[test]
fn a_damaged_file_giving_two_layers_one_id_is_refused() {
    let mut app = rich_app();
    let id = app.canvas.layers[1].id;
    app.canvas_mut().layers[2].id = id;
    let err = decode_project(&encode_project_data(&ProjectSnapshot::capture(&app)).unwrap())
        .err()
        .unwrap();
    assert!(err.contains("layer ids"), "{err}");
}

#[test]
#[ignore = "fuzzing"]
fn fuzz_project() {
    let app = rich_app();
    open(&encode_project(&app).unwrap());
    let bare = encode_project_data(&ProjectSnapshot::capture(&app)).unwrap();
    crate::fuzz::fuzz("rpainter", &bare, std::time::Duration::from_secs(2), open);
}
