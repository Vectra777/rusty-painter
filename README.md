# Rusty Painter

A lightweight linux desktop painting playground built with Rust and `eframe/egui`. The goal of Rusty Painter is to explore fast 2D painting techniques (tiling, atlases, multithreaded brushes) while keeping the UI simple.

![](/imgs/2025-12-1118-06-17-ezgif.com-video-to-gif-converter.gif)

## Features
- **Brush Engine**: Soft, hard, pixel, bristle, sketch, hatching and image-tip brushes (mipmapped, any picture; several tips per brush, taken in turn, at random, or by pressure or direction) with spacing, flow, scatter, stabilizer and airbrush (keeps painting while the pen rests). Dynamics: tapers in and out (no lag), stroke speed, tip angle / squash / follow the stroke or the pen's tilt, size, opacity, spray and colour randomness, a pressure curve per setting, tilt → size and opacity. Paper textures (multiply, subtract, height), a dual brush (a second tip masking the first), watercolour edges, decoration brushes in the picture's own colours, ribbons laid along the stroke, all 27 blend modes per brush, and a wet mixing smudge. Ready-made brushes (pencil, ink, calligraphy, charcoal, chalk, bristles, spray, foliage, glow…). Any input (pressure, speed, tilt, direction, distance, time, randomness per dab or per stroke) can drive size, opacity, angle, squash, hue, saturation or value through its own curve (Brush → Inputs). Inputs can also drive texture strength, hardness, scatter and the mix with the secondary colour. Texture grain can be turned, move with the stroke, shift each stroke or apply to each dab. Stabiliser: simple, dynamic, pulled string (lazy mouse), post-correction or motion filter. The Blur tool's Filter mode paints any filter through the brush. Make a tip from the selection (Edit → Define Brush Tip from Selection). Save your own presets and share them as `.rpbrush` files; import brushes from GIMP (`.gbr`, `.gih`), Photoshop (`.abr`), Krita (`.kpp`, `.bundle`), MyPaint (`.myb`) and Clip Studio (`.sut`). The presets window has tags, favourites, search, recent brushes, and a pop-up palette around the pointer (hold `K`, or right-click the canvas). See [docs/brush-comparison.md](docs/brush-comparison.md) for how it compares with Krita, Clip Studio and ibisPaint.
- **Tablet Support**: Pen pressure, tilt and the eraser end on Windows (Windows Ink, via `octotablet`) and Android (every stylus sample, with a resting palm ignored while the pen is down). On Linux, X11 tablets (Wacom and libinput drivers) give pressure, tilt and the eraser through XInput2, and Wayland tablets work with `RUSTY_PAINTER_ENABLE_WAYLAND_TABLET=1`.
- **Layers**: Full layer support with visibility, opacity, blending, folders, masks, clipping masks (Ctrl+Alt+G: a layer shows only where the one below has paint) and adjustment layers (Filter → New Adjustment Layer: brightness/contrast, hue/saturation, levels, curves, colour balance, gradient map, invert, desaturate, posterize or threshold over everything below, live). Merge Down (Ctrl+Alt+E), Merge Visible (Ctrl+Shift+E) and Flatten Image, as shown on screen. Lock a layer's position; mark it as a draft (shown, but left out of export and merging) or as a reference for fills, the magic wand and colour range.
- **Filters**: Brightness/contrast, hue/saturation, levels, curves (a master curve and one per channel), colour balance (shadows, midtones, highlights), gradient map (any colours, with presets), invert, desaturate, posterize, threshold, Gaussian and motion blur, sharpen, noise, pixelate, and Extract Line Art (a scanned drawing's paper turns transparent), previewed live on the layer or the selection. Sliders preview smoothly even on a whole 4K canvas.
- **Image menu**: canvas size (with an anchor), image size (smooth or hard pixels), crop to selection, rotate and flip the whole document, each one undo step.
- **Text**: the Text tool types onto a text layer in any shipped or system font, with size, alignment, spacing and colour; drag it into place before keeping it. The text stays editable: click it with the Text tool or double-click the layer. Painting on it turns it into pixels (Layer → Rasterise Text does it on purpose).
- **QuickShape**: hold the pen still at the end of a stroke and it becomes a clean line, ellipse (turned the way it was drawn), rectangle or polygon, editable before it's applied.
- **Time-lapse**: File → Record Time-lapse keeps a frame after each change; export it as MP4 (with `ffmpeg` installed) or GIF.
- **Autosave**: unsaved work is kept once the app is idle, and offered back after a crash or a quit without saving.
- **Selection Tools**: Rectangle, ellipse, lasso (with smoothing), polygon, magnetic lasso, magic wand, colour range and selection brush; replace, add, erase and intersect; invert; every change undoable. The Transform tool moves, scales, rotates, puts in perspective or distorts the selection outline itself when there's nothing under it. Select → Modify grows, shrinks, feathers or borders the selection; Ctrl+click a layer's thumbnail selects its paint (Shift adds, Alt subtracts); selections can be saved by name with the project and loaded back; Quick Mask (Shift+Q) paints the selection with the brush and eraser.
- **Viewing Aids**: View → Grid (square or isometric, with a pixel grid from 800%), guides (Ctrl+drag from beside the canvas, or View → Guides) with Snap to Guides / Snap to Grid, a Reference Image window to pick colours from, and a Navigator showing the whole canvas.
- **Wrap Around**: View → Wrap Around paints across the canvas's edges and shows it repeated, for seamless tiles.
- **Mirror Painting**: Left/right, top/bottom, four-way or radial (mandala, optionally kaleidoscope) around a centre and angle you drag on the canvas; flip the view with H.
- **Shapes, Ruler & Assistants**: Line, rectangle, ellipse and polygon drawn with the brush, filled, or both, editable until applied; a ruler that straightens strokes along or parallel to it; vanishing-point, perspective, ellipse and concentric assistants strokes snap to (View → Assistants, or the Shape menu). Guides are saved with the project.
- **Gradients**: Linear, radial, reflected and angle gradients, previewed live, dithered, clipped to the selection.
- **Smart Patch**: Content-aware fill of the selection from its surroundings (PatchMatch), or paint over something with the selection brush to remove it.
- **Transform Tools**: Move, rotate, scale and flip selections or layers with a live preview; **Perspective** (drag the corners of a plane seen at an angle) and **Distort** (a grid of points, 1×1 to 5×5 cells, inner points included: the picture bends smoothly through them). The selection follows what was transformed.
- **History**: One undo history for the whole document (pixels, selections, transforms, layer add/remove/reorder/merge, canvas resizes; all of it kept in saved files), undone in the order things were done whichever layer is selected.
- **Canvas**: Large canvas support (default 4000x4000) backed by tiled storage and GPU texture atlases.
- **Project Files**: Save/open your work as a `.rpainter` project (compressed, includes undo history) via the Open/Save buttons in the top bar. File managers show a preview of the drawing, and Krita, GIMP or MyPaint can open it as a flattened image.
- **Export**: Save your work as PNG, JPEG, TIFF, or a layered PSD. Photoshop files (`.psd`, 8-bit RGB or grayscale) open with their layers, folders, masks, blend modes and clipping.
- **Performance**: Optional masked brush mode and zoom-out LOD for performance experiments.
- **Android** (experimental): Runs on Android via a patched `winit` and native activity glue — see [Android (APK build)](#android-apk-build) below.

## Quick Start
Prerequisites: Rust toolchain (`cargo`, `rustc`) installed.

```bash
cargo run --release
```

That launches the native egui window with the default canvas and brush settings.

### Where your data is kept
Presets, settings, swatches, gradients, the autosave and any brush tips you add live in one folder per user, whichever folder the app is started from:

| System | Folder |
|---|---|
| Linux | `$XDG_DATA_HOME/rusty-painter`, else `~/.local/share/rusty-painter` |
| Windows | `%APPDATA%\rusty-painter` |
| macOS | `~/Library/Application Support/rusty-painter` |
| Android | the app's private storage |

Set `RUSTY_PAINTER_DATA` to use another folder (a portable install, or a clean one to try things in). Delete the folder to start fresh.

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

Wall-clock timings drift with the machine's state (a few percent, sometimes much more). To compare two builds exactly, count instructions on the fixed workloads in `examples/fixed_workloads.rs`, ideally in single-codegen-unit builds so inlining doesn't shift between them:
```bash
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 cargo build --release --example fixed_workloads
perf stat -e instructions:u target/release/examples/fixed_workloads plain_stroke
```

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

- **Paint**: left drag (pen pressure where supported). `B` brush, `E` eraser, `[` / `]` size, `K` (hold) or right-click the pop-up brush palette.
- **View**: `Space` + drag or right drag to pan, middle drag to rotate, wheel to zoom, `Ctrl+0` fit, `Ctrl+1` actual pixels, `H` flip, `Ctrl+'` grid, `Ctrl+;` guides (Ctrl+drag a guide to move it). Two fingers pan, pinch and twist.
- **Tools**: `M` rectangle/ellipse select, `L` lasso (again: polygon, magnetic), `Q` magic wand, `Shift+Q` quick mask, `U` shapes, `Shift+G` gradient, `V`/`T` transform, `I` eyedropper, `G` fill (again: enclose, lasso delete), `W` liquify, `S` smudge/blur (smudge also deforms or clones: Ctrl+click the clone source; blur also sharpens or adjusts colour), `R` ruler (assistants: View → Assistants).
- **Edit**: `Ctrl+Z` undo, `Ctrl+Shift+Z` / `Ctrl+Y` redo (one history for the whole document), `Ctrl+X` / `Ctrl+C` / `Ctrl+V` cut / copy / paste (pastes as a new layer, ready to transform; images from other programs too), `Ctrl+Shift+C` copy merged, `Ctrl+J` duplicate layer, `Ctrl+Alt+E` merge down, `Ctrl+Shift+E` merge visible, `Ctrl+A` select all, `Ctrl+D` / `Esc` deselect, `Ctrl+Shift+I` invert, `Delete` erase the selected pixels, `Shift+F5` content-aware fill, `Enter` / `Esc` apply / cancel a transform.
- **File**: `Ctrl+N` new, `Ctrl+O` open, `Ctrl+S` save, `Ctrl+E` export, `Ctrl+Shift+O` import an image as a layer, or drop image files on the window (on the Palette window, a dropped picture gives its colours instead).

## Project Files
Work is saved as a single `.rpainter` file via **Open**/**Save** in the top bar — layers, tile data, and undo history all round-trip. The file is an [OpenRaster](https://www.openraster.org/) archive: a ZIP with `mergedimage.png` (the flattened picture) and `Thumbnails/thumbnail.png`, which is what file managers such as Dolphin use for the preview. The project itself is one more entry, `rusty-painter/project.rpnt`, in a versioned binary format (`src/project/`): tile pixel data is zstd-compressed per tile, and layers are matched up by a stable id (not position) so undo stays correct even if you'd reordered layers before saving. Older project files (including the bare binary files saved before the OpenRaster container) remain loadable after format additions — new fields default sensibly on read rather than breaking the load.

## UI Panels
The canvas gets the screen: everything else is one thin bar and two rails, with panels that slide out when you want them.
- **Top bar**: the menus (File, Edit, Image, Layer, Select, Filter, View, Help), the active tool's options, and zoom / fit / ruler / flip on the right. In touch mode: the menu sheet, finger painting, undo and redo.
- **Tool strip** (left edge): the tools; its bottom button opens the brush settings panel.
- **Right rail**: the brush colour (opens the colour panel) and layers. Tab hides or shows every panel. On a phone-sized window panels float over the canvas, one at a time, and a tap on the canvas closes them.
- **Notices**: messages (export done, errors) appear briefly over the canvas's corner.
- **Brush Settings**: Brush type (soft, pixel, bristle, sketch, hatching), size, hardness, flow, spacing, jitter, airbrush, tips (several, colour, ribbon), dynamics, texture, dual brush, watercolour edges, pressure and tilt, stabilizer, pixel-perfect mode, AA.
- **Color Picker**: Triangle HSVA picker with opacity slider.
- **Brush Presets**: Quick presets; selecting one keeps your current color. Right-click a preset to export, delete or tag it; star it as a favourite; search by name or tag, or filter by tag, favourites or recent. The ☰ menu imports and exports sets.
- **Layers**: Add/remove layers, drag to reorder, toggle visibility, set opacity, choose active layer; lock position, draft and reference toggles; Ctrl+click a thumbnail to select its paint; double-click a text layer to edit it.
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

## License
GPL-3.0-only: see [LICENSE](LICENSE).

## Contributing
The project is early-stage and focused on performance experiments. If you have ideas for improving brush quality, tiling performance, or UI/UX, feel free to open an issue or directly contact me. Tests and benchmarks are especially welcome.
