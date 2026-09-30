# Architecture

## Crates and entry points

There is one library crate, `rusty_painter`, in `src/lib.rs`:

- The desktop binary, `src/main.rs`, only calls `rusty_painter::run()`, which opens the eframe window with the wgpu renderer.
- On Android the app starts at `android_main` in `lib.rs`. It uses the patched `winit` in `vendor-winit/` and the JNI glue in `src/android.rs`.
- The benchmarks in `benches/` link the same library. `benches/app_bench.rs` also needs the `bench` feature, which exposes `bench_api`: headless entry points that drive a real `PainterApp`.

## Module map

Modules only depend on the ones below them:

```
app ──────────────┐        the application: state, frame loop, input, tools, view
ui ───────────────┤        egui panels; reads and changes PainterApp
                  ▼
project   brush_engine   selection   tablet
   │           │            │
   └───────────┴─────┬──────┘
                     ▼
                  canvas             document model and pixel algorithms
```

| Module | Responsibility |
| --- | --- |
| `canvas::storage` | `Canvas`, made of `Layer`s, which are made of lazily allocated 64 px tiles. `composite` blends layers for display and export, `pixels` holds the pixel writers that record undo, and `transform` holds affine and perspective moves plus floating selections. |
| `canvas::shader` | Shader layers without the GPU: `ShaderLayer`, the GLSL wrapper and compiling (naga), and `live_layout`, which splits the stack into runs around the visible shader layers. The GPU side is `app::view::shader_gpu`; playback and baking are `app::shader_ops`. |
| `canvas::history` | Undo history, one per layer. `UndoAction` holds tile snapshots plus changes to the selection, the transform and the layer tree. |
| `canvas::{fill, gradient, inpaint, liquify, palette, blend, blend_modes}` | Pure pixel algorithms and colour maths. They don't know about the app. |
| `brush_engine` | Dabs, tips, spacing and pressure (`stroke`), the stabiliser, mirror painting (`symmetry`), and the stroke worker thread. The engines beside the plain dab: `bristle`, `sketch`, `hatching`, ribbons (`Brush::ribbon`); `dual` (a second tip masking the first), `wet_edge` (watercolour edges when the pen lifts). `preset_file` reads and writes `.rpbrush` files; `import` reads other apps' brushes. |
| `selection` | Selection shapes and per-pixel masks, how they combine, the magnetic lasso, and transform handles. |
| `project` | The `.rpainter` format (`encode_project` / `decode_project`) and image export. |
| `tablet` | Pen samples with pressure, from octotablet (Windows Ink, Wayland). |
| `app` | `PainterApp`: its state (`state.rs`), the frame loop (`painter.rs`), input routing (`input/`), one module per tool (`tools/`), and display (`view/`). |
| `ui` | The top bar (menus, tool options, view controls), the tool strip, panels and dialogs; `app/layout.rs` places the right rail and the side panels. Style tokens are in `ui/style.rs`; `ui/theme.rs` applies them. |

## Data model

- **Canvas**
  - `Canvas { layers, active_layer_idx, width, height, clear_color, blend_space, ... }`.
  - Layers form a flat list. Folders (`LayerKind::Group`) and masks are entries in that list too. The tree comes from `Layer::parent` and the mask `owner` links; among siblings, list order is stacking order.
- **Tiles**
  - Each layer maps `(tx, ty)` to `Arc<Mutex<TileCell>>`. A cell is `None` until something paints there, and `is_empty` lets compositing skip transparent tiles.
  - Tile contents have their own locks, so the UI can composite while the stroke worker paints.
- **Pixels**
  - `Color32`: sRGB-encoded, premultiplied.
  - Compositing works in linear light or in gamma space, depending on `canvas.blend_space`.
- **Identity**
  - `LayerId` is stable when layers are reordered.
  - Anything that outlives a frame (undo, sessions, saved files) refers to a layer by id, not by index.
- **Per-layer app state**
  - `LayerState::histories` has exactly one `History` per layer, in layer order.
  - `canvas_ops::debug_assert_layer_state_in_sync` checks this in debug builds.
  - Every layer insert, remove or move goes through `canvas_ops`, which keeps the two in step.

## Invariants

1. **Only one writer changes the canvas structure.**
   - While a stroke is painting, the stroke worker holds an `Arc<Canvas>`.
   - Anything that changes layers, undoes or loads must call `PainterApp::canvas_mut()` or `release_canvas()` first. That ends the stroke and waits for the worker to go idle.
   - `stroke_ops::exclusive` then turns the `Arc` into `&mut Canvas`. It panics if the worker still holds a reference.
2. **Replacing the document ends everything tied to the old one.**
   - `canvas_ops::replace_document` is the only way to swap documents. New Canvas and Open both use it.
   - In order, it:
     1. stops background work (the stroke worker and the smart patch task);
     2. ends tool sessions (selection, transform, shape, gradient, fill path, guides drag);
     3. installs the canvas with fresh per-layer state;
     4. clears the selection and resets the view.
   - Tests: `canvas_ops::document_tests`.
3. **A tool's session ends when you leave its tool.**
   - `PainterApp::settle_tool_sessions` runs every frame before input.
   - A transform, gradient, shape or liquify that is still pending gets applied. A magnetic outline gets dropped.
4. **One user action is one undo step.**
   - Pixel writers take a `&mut UndoAction` and snapshot each tile the first time they touch it.
   - The caller pushes that action once, onto the active layer's `History`.

## Pipelines

### A frame (`PainterApp::update`, `app/painter.rs`)

`update` runs these stages in order. Each stage is a method or a call listed below.

1. **Setup**
   - `apply_touch_mode` sets the theme and touch metrics.
   - `fit_panels_to_screen` sizes the panels.
   - Keyboard shortcuts are handled.
   - `poll_export` checks the export task.
   - Layer thumbnails are refreshed.
2. **Chrome.** The top bar (and the menu sheet on touch screens), the tool strip, the right rail, and the side panels that are open.
3. **Canvas.**
   - `place_view` keeps the view fitted and centred.
   - `render::draw_canvas` allocates the canvas area.
   - Pen samples are polled, then touch gestures are handled.
4. **Tools.**
   - `settle_tool_sessions` runs first.
   - Then, unless a smart patch is running, `input::handle_input` routes press, drag and release to the active tool.
   - Tool previews update: the transform overlay, the gradient, and liquify while held.
5. **Pixels.**
   - The stroke worker gets up to 5 ms to catch up.
   - `sync_stroke_worker` collects the tiles it painted and its finished undo steps.
   - `shader_tick` compiles changed shader layers, advances their clocks, and works out whether they show live (`canvas::shader::live_layout`).
   - `render::update_dirty_textures` composites dirty tiles and uploads them to the GPU atlases (one set per run when shader layers show live).
   - `render::paint_canvas` draws them.
   - `draw_overlays` draws the selection, guides, shapes, gradient handles and the transform box.
6. **Windows.** `show_windows` shows the dialogs and floating windows.

### A brush stroke

1. **Start.** `input` calls `PainterApp::start_stroke_with_pressure` (`app/stroke_ops.rs`).
   - This captures a `StrokeSetup`: the canvas `Arc`, the brush, the selection, the thread pool, the layer and the symmetry settings.
   - It hands the setup to `StrokeWorker::begin`.
2. **Samples.** Each sample goes to the worker thread through `add_stroke_point`, after the ruler or an assistant snaps it (`app/tools/guides.rs`, `assistants.rs`; a stroke round an ellipse gets extra samples along the curve).
3. **Dabs.** On the worker:
   - `StrokeState` spaces the dabs and interpolates pressure. An airbrush brush also gets dabs from the worker's timer while the pen rests (`StrokeState::airbrush`).
   - The stabiliser smooths the path.
   - Brushes whose dabs differ (dynamics, several tips, bristles, sketch, hatching, colour tips) plan each dab (`DabVar`: size, strength, turn, tip, colour); a ribbon brush makes segments instead.
   - `StrokeContext` expands mirror copies (and, with wrap-around, copies across the canvas edges) and paints them all as one batch with `Brush`. Tiles are painted in parallel on the pool.
   - Dabs build up coverage in per-tile `StrokeBuffer`s, which are resolved over the tile as it was before the stroke, so pixels aren't quantised again between dabs. A dual brush's second tip builds a mask there too, combined with the coverage when resolving; watercolour edges rework the whole stroke's coverage when the pen lifts.
4. **Hand-off.** Each frame, `sync_stroke_worker` marks the painted tiles for redraw. When the stroke ends, it pushes the stroke's `UndoAction` onto that layer's history.

### Undo and redo

- `PainterApp::apply_history` (`app/painter.rs`) first ends any stroke, liquify or transform, so it becomes a normal step.
- It then takes the active layer's `History` and calls `undo` or `redo` with `&mut Canvas`.
- Each `TileSnapshot` swaps its pixels with the tile's, so the same record serves both directions. Selection and layer-tree changes swap the same way.
- Older steps are compressed with zstd. Each layer keeps at most `MAX_UNDO_BYTES` of snapshots.

### Save and open

- **Save.**
  - `save_project_to_path` ends any stroke (`release_canvas`) and calls `encode_project`.
  - `encode_project` writes a versioned JSON header, then a blob area (`project/blobs.rs`).
  - The blob area holds the tiles and every undo step, compressed with zstd in parallel, plus a PNG thumbnail.
- **Open.**
  - `load_project_from_path` calls `decode_project`, then `replace_document`, then restores the colour model and selects the brush.
  - Files from older versions still load: fields added since then get default values.
- **Export.** Export runs on its own thread (`ExportState::task`). The frame loop polls it in `poll_export`.

### Background work

| Work | Thread | How the UI waits |
| --- | --- | --- |
| Brush strokes | the stroke worker, which paints on the rayon pool | `wait_idle_for` a 5 ms budget per frame; `release_canvas` waits fully |
| Export | a spawned thread | `poll_export` each frame |
| Smart patch | a spawned thread (`app/tools/patch.rs`) | `poll_patch` each frame; input is paused while it runs |
| Fills, gradients, transforms, compositing | rayon, inside the call | synchronous |
