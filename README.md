# Rusty Painter

A lightweight linux desktop painting playground built with Rust and `eframe/egui`. The goal of Rusty Painter is to explore fast 2D painting techniques (tiling, atlases, multithreaded brushes) while keeping the UI simple.

![](/imgs/2025-12-1118-06-17-ezgif.com-video-to-gif-converter.gif)

## Features
- **Brush Engine**: Soft, hard, pixel, bristle, sketch, hatching and image-tip brushes (mipmapped, any picture; several tips per brush, taken in turn, at random, or by pressure or direction) with spacing, flow, scatter, stabilizer and airbrush (keeps painting while the pen rests). Dynamics: tapers in and out (no lag), stroke speed, tip angle / squash / follow the stroke or the pen's tilt, size, opacity, spray and colour randomness, a pressure curve per setting, tilt → size and opacity. Paper textures (multiply, subtract, height), a dual brush (a second tip masking the first), watercolour edges, decoration brushes in the picture's own colours, ribbons laid along the stroke, all 27 blend modes per brush, and a wet mixing smudge. Ready-made brushes (pencil, ink, calligraphy, charcoal, chalk, bristles, spray, foliage, glow…). Save your own presets and share them as `.rpbrush` files. See [docs/brush-comparison.md](docs/brush-comparison.md) for how it compares with Krita, Clip Studio and ibisPaint.
- **Tablet Support**: Pen pressure, tilt and the eraser end on Windows (Windows Ink, via `octotablet`) and Android (every stylus sample, with a resting palm ignored while the pen is down). On Linux, Wayland tablets work with `RUSTY_PAINTER_ENABLE_WAYLAND_TABLET=1`; X11 has no pressure yet.
- **Layers**: Full layer support with visibility, opacity, and blending.
- **Selection Tools**: Rectangle, ellipse, lasso (with smoothing), polygon, magnetic lasso, magic wand, colour range and selection brush; replace, add, erase and intersect; invert; every change undoable. The Transform tool moves, scales, rotates, puts in perspective or distorts the selection outline itself when there's nothing under it.
- **Mirror Painting**: Left/right, top/bottom, four-way or radial (mandala, optionally kaleidoscope) around a centre and angle you drag on the canvas; flip the view with H.
- **Shapes & Ruler**: Line, rectangle, ellipse and polygon drawn with the brush, filled, or both, editable until applied; a ruler that straightens strokes along or parallel to it.
- **Gradients**: Linear, radial, reflected and angle gradients, previewed live, dithered, clipped to the selection.
- **Smart Patch**: Content-aware fill of the selection from its surroundings (PatchMatch), or paint over something with the selection brush to remove it.
- **Transform Tools**: Move, rotate, scale and flip selections or layers with a live preview; **Perspective** (drag the corners of a plane seen at an angle) and **Distort** (a grid of points, 1×1 to 5×5 cells, inner points included: the picture bends smoothly through them). The selection follows what was transformed.
- **History**: One undo history for the whole document (pixels, selections, transforms, layer add/remove/reorder/merge), undone in the order things were done whichever layer is selected.
- **Canvas**: Large canvas support (default 4000x4000) backed by tiled storage and GPU texture atlases.
- **Project Files**: Save/open your work as a `.rpainter` project (compressed, includes undo history) via the Open/Save buttons in the top bar. File managers show a preview of the drawing, and Krita, GIMP or MyPaint can open it as a flattened image.
- **Export**: Save your work as PNG, JPEG, or TIFF.
- **Performance**: Optional masked brush mode and zoom-out LOD for performance experiments.
- **Android** (experimental): Runs on Android via a patched `winit` and native activity glue — see [Android (APK build)](#android-apk-build) below.

## Quick Start
Prerequisites: Rust toolchain (`cargo`, `rustc`) installed.

```bash
cargo run --release
```

That launches the native egui window with the default canvas and brush settings.

## Android (APK build)
This is an experimental setup and may need platform fixes. Building for Android needs more than the target and `cargo-apk` — this repo vendors and patches `winit` and ships its own NDK linker wrappers, both required for the build to work:

- `vendor-winit/` — a patched copy of `winit`, pulled in via `[patch.crates-io]` in `Cargo.toml`, needed for Android windowing/lifecycle support beyond what upstream `winit` provides out of the box.
- `scripts/linkers/` + `scripts/build-android.sh` — wrapper scripts that route each Android target's linker invocation through the NDK toolchain; `.cargo/config.toml` points cargo at them. Run `scripts/build-android.sh` rather than a bare `cargo apk build` to make sure these are picked up.
- `src/android.rs` — the native JNI/lifecycle glue (`android_logger`, `jni`, `ndk-context`) that makes the app actually run once launched, as opposed to just compiling for the target.

To build:

```bash
rustup target add aarch64-linux-android
cargo install cargo-apk
scripts/build-android.sh
```

If you have multiple Android SDK/NDK installs, ensure your environment points to the intended one (e.g. `ANDROID_HOME` and `ANDROID_NDK_HOME`).

**Release signing** is not committed. `cargo-apk` signs release builds with the keystore given by two environment variables:

```bash
export CARGO_APK_RELEASE_KEYSTORE=/path/to/release.keystore
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=...
scripts/build-android.sh aarch64-linux-android release   # -> target/release/apk/rusty-painter.apk
```

(The commented-out `[package.metadata.android.signing.release]` block in `Cargo.toml` works too, but must never be committed.)

## Benchmarks
```bash
cargo bench --features bench --bench app_bench               # every tool end to end on a painted 4000 px canvas
cargo bench --bench tools_bench                              # tool engines: mirror painting, shapes, selections, gradients, smart patch
cargo bench --bench brush_bench                              # brush stamping and compositing
cargo bench --bench tools_bench -- --save-baseline before    # save a run...
cargo bench --bench tools_bench -- --baseline before         # ...and compare a later one against it
```
Heavier single-shot timings live in ignored tests: `cargo test --release -- --ignored --nocapture`.

## Profiling
```bash
scripts/flamegraph.sh            # use the app, close the window -> flamegraph.svg
FREQ=199 scripts/flamegraph.sh   # fewer samples
```
Needs `perf` and `cargo install flamegraph`. The script builds with frame pointers into `target/profiling` and records with `--call-graph fp --no-inline`. Plain `cargo flamegraph` uses DWARF call graphs and inline resolution, which here wrote a large `perf.data` and spent ~7 minutes at full CPU turning a 10 s recording into a graph.

## CI and releases
- **CI** (`.github/workflows/ci.yml`) runs on every push to `master` and on pull requests: `cargo fmt --check`, `clippy -D warnings`, tests, a bench build, a docs build, an Android compile check, and `cargo audit`.
- **Release** (`.github/workflows/release.yml`) runs the same checks first, then builds and publishes:
  - **By hand:** GitHub → *Actions* → *Release* → *Run workflow*. Enter the version and tick the platforms (Linux, Windows, Android). Untick *Publish* to only build; the files are then downloadable from the run page.
  - **By tag:** `git tag v0.2.0 && git push origin v0.2.0` builds all three and publishes release `v0.2.0`.
  - Assets: `rusty-painter-<version>-linux-x86_64.tar.gz`, `-windows-x86_64.zip`, `-android-arm64.apk`, plus a source zip.
- **Android signing in CI:** add two repository secrets (*Settings → Secrets and variables → Actions*):
  - `ANDROID_KEYSTORE_BASE64`: `base64 -w0 release.keystore`
  - `ANDROID_KEYSTORE_PASSWORD`: its password

  Without them the APK is signed with a throwaway key: it installs, but Android won't let a later release update it (different signature), so set them before sharing APKs. Keep the keystore safe: losing it means users must uninstall to upgrade.

## Controls
The full list is in **Help → Keyboard Shortcuts**, labelled for your keyboard (QWERTY, AZERTY or QWERTZ, detected or set in Settings): letter shortcuts follow the letter, digit and symbol ones the key's position. The main ones (QWERTY names):

- **Paint**: left drag (pen pressure where supported). `B` brush, `E` eraser, `[` / `]` size.
- **View**: `Space` + drag or right drag to pan, middle drag to rotate, wheel to zoom, `Ctrl+0` fit, `Ctrl+1` actual pixels, `H` flip. Two fingers pan, pinch and twist.
- **Tools**: `M` rectangle/ellipse select, `L` lasso (again: polygon, magnetic), `Q` magic wand, `U` shapes, `Shift+G` gradient, `V`/`T` transform, `I` eyedropper, `G` fill (again: enclose, lasso delete), `W` liquify, `S` smudge/blur, `R` ruler.
- **Edit**: `Ctrl+Z` undo, `Ctrl+Shift+Z` / `Ctrl+Y` redo (one history for the whole document), `Ctrl+X` / `Ctrl+C` / `Ctrl+V` cut / copy / paste (pastes as a new layer, ready to transform; images from other programs too), `Ctrl+Shift+C` copy merged, `Ctrl+J` duplicate layer, `Ctrl+A` select all, `Ctrl+D` / `Esc` deselect, `Ctrl+Shift+I` invert, `Delete` erase the selected pixels, `Shift+F5` content-aware fill, `Enter` / `Esc` apply / cancel a transform.
- **File**: `Ctrl+N` new, `Ctrl+O` open, `Ctrl+S` save, `Ctrl+E` export, `Ctrl+Shift+O` import an image as a layer, or drop image files on the window (on the Palette window, a dropped picture gives its colours instead).

## Project Files
Work is saved as a single `.rpainter` file via **Open**/**Save** in the top bar — layers, tile data, and undo history all round-trip. The file is an [OpenRaster](https://www.openraster.org/) archive: a ZIP with `mergedimage.png` (the flattened picture) and `Thumbnails/thumbnail.png`, which is what file managers such as Dolphin use for the preview. The project itself is one more entry, `rusty-painter/project.rpnt`, in a versioned binary format (`src/project/`): tile pixel data is zstd-compressed per tile, and layers are matched up by a stable id (not position) so undo stays correct even if you'd reordered layers before saving. Older project files (including the bare binary files saved before the OpenRaster container) remain loadable after format additions — new fields default sensibly on read rather than breaking the load.

## UI Panels
- **Menus and options bar**: File/Edit/View/Help on desktop (a slide-up sheet on tablets); under them, the active tool's options.
- **Toolbar**: the tools on the left edge.
- **Brush Settings**: Choose brush type/mode, size, hardness, flow, spacing, jitter, stabilizer, pixel-perfect mode, AA.
- **Color Picker**: Triangle HSVA picker with opacity slider.
- **Brush Presets**: Quick presets; selecting one keeps your current color. Right-click a preset to export or delete it; the ☰ menu imports and exports sets.
- **Layers**: Add/remove layers, drag to reorder, toggle visibility, set opacity, choose active layer.
- **General Settings**: Toggle masked brush (fast), high-quality zoom out (slower), adjust brush thread count.
- **Export**: File → Export, or `Ctrl+E`.

## Project Structure
```
rusty-painter/
├── src/            the application and engine (detailed below)
├── benches/        criterion benchmarks (app, tools, brush, blend)
├── examples/       standalone experiments (stroke cost)
├── docs/wiki/      architecture and developer guide
├── scripts/        Android build, NDK linker wrappers, flamegraph
├── vendor-winit/   patched winit for Android
├── imgs/           README media
└── .github/        CI and release workflows
```

One library crate (`src/lib.rs`); `src/main.rs` only calls `rusty_painter::run()`, and Android enters at `android_main`. Modules depend downwards: `app` and `ui` use the engine modules, never the other way round.

```
src/
├── app/                 the application: PainterApp and everything it does
│   ├── painter.rs       PainterApp and the frame loop (setup → chrome → canvas → tools → pixels → windows)
│   ├── state.rs         app state grouped by concern (brush, viewport, layers, workspace...)
│   ├── document.rs      canvas settings and limits, tile/atlas sizes
│   ├── canvas_ops.rs    replace the document, layer add/remove/move, redraw marking
│   ├── stroke_ops.rs    brush strokes via the stroke worker; exclusive canvas access
│   ├── input/           pointer, pen and touch routing to the active tool; keyboard shortcuts
│   ├── tools/           one module per tool (select, shape, gradient, fill, transform, liquify, patch...)
│   └── view/            GPU canvas, tile upload and compositing for display, view transform
├── canvas/              the document model and pixel algorithms
│   ├── storage/         Canvas, layers and tiles; compositing; pixel writers; transforms
│   ├── history.rs       per-layer undo (tile snapshots, selection and layer tree changes)
│   └── blend*.rs, fill.rs, gradient.rs, inpaint.rs, liquify.rs, palette.rs
├── brush_engine/        dabs, tips, strokes, stabiliser, mirror painting, the stroke worker thread
├── selection/           selection shapes and masks, magnetic lasso, transform state
├── project/             .rpainter save/open, image export
├── tablet/              pen input (octotablet: Windows Ink, Wayland)
├── ui/                  egui panels, menus, tool options, theme (style.rs tokens, theme.rs)
├── android.rs           Android platform glue
└── bench_api.rs         headless entry points for benches/app_bench.rs (feature `bench`)
```

The [developer guide](docs/wiki/Developer-Guide.md) says where a typical change goes and how to test it; [architecture](docs/wiki/Architecture.md) covers the data model and pipelines.

## Development
The checks CI runs:
```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo check --locked --benches --features bench
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items
```

## Contributing
The project is early-stage and focused on performance experiments. If you have ideas for improving brush quality, tiling performance, or UI/UX, feel free to open an issue or directly contact me. Tests and benchmarks are especially welcome.
