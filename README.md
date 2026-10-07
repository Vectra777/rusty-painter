# Rusty Painter

A lightweight linux desktop painting playground built with Rust and `eframe/egui`. The goal of Rusty Painter is to explore fast 2D painting techniques (tiling, atlases, multithreaded brushes) while keeping the UI simple.

![](/imgs/2025-12-1118-06-17-ezgif.com-video-to-gif-converter.gif)

## Features
- **Brush Engine**: Soft, hard, pixel, bristle, sketch, hatching and image-tip brushes (mipmapped, any picture; several tips per brush, taken in turn, at random, or by pressure or direction) with spacing, flow, scatter, stabilizer and airbrush (keeps painting while the pen rests). Dynamics: tapers in and out (no lag), stroke speed, tip angle / squash / follow the stroke, the pen's tilt or its barrel rotation, tips flipped at random, size, opacity, spray and colour randomness, a pressure curve per setting (pressure can also set the spacing) on top of the pen's own curve, which can be fitted to a few test strokes (Settings → Calibrate), tilt → size and opacity, hard edges from any tip. Paper textures (multiply, subtract, height, colour dodge, hard mix), a dual brush (a second tip masking the first), watercolour edges, decoration brushes in the picture's own colours, ribbons laid along the stroke, all 28 blend modes per brush (Photoshop's and Krita's Parallel), and colour mixing: any brush can pick up the paint under it and mix in its own colour, like Krita's Colour Smudge, with all its settings (tip, angle, texture, inputs, scatter, stabiliser, airbrush). Wet paint: washes that spread, darken at their edges, drip and dry, and water that lifts dry paint (Brush → Wet paint). Impasto: paint laid thick, lit by the layer's light (Brush → Impasto, Layer → Impasto). Engines after Krita's: spray (particle clouds), chalk, curve, grid, tangent normal (normal maps from the pen's tilt) and particle (a swarm the pen pulls along); Krita presets of these import as them. Ready-made brushes (pencil, ink, calligraphy, charcoal, chalk, bristles, spray, foliage, glow…). Any input (pressure, speed, tilt, direction, distance, time, randomness per dab or per stroke, barrel rotation, an airbrush's wheel) can drive size, opacity, angle, squash, hue, saturation or value through its own curve (Brush → Inputs). Inputs can also drive texture strength, hardness, scatter and the mix with the secondary colour. Texture grain can be turned, move with the stroke, shift each stroke or apply to each dab. Stabiliser: simple, dynamic, pulled string (lazy mouse), post-correction (when the pen lifts, or while drawing) or motion filter. The Blur tool's Filter mode paints any filter through the brush. Make a tip from the selection (Edit → Define Brush Tip from Selection). Save your own presets and share them as `.rpbrush` files (with their tags and favourite star); import brushes from GIMP (`.gbr`, `.gih`), Photoshop (`.abr`), Krita (`.kpp`, `.bundle`), MyPaint (`.myb`) and Clip Studio (`.sut`). The presets window has tags, favourites, search, recent brushes, and a pop-up palette around the pointer (hold `K`, or right-click the canvas). See [docs/brush-comparison.md](docs/brush-comparison.md) for how it compares with Krita, Clip Studio and ibisPaint.
- **Tablet Support**: Pen pressure, tilt and the eraser end on Windows (Windows Ink, via `octotablet`) and Android (every stylus sample, with a resting palm ignored while the pen is down). On Linux, X11 tablets (Wacom and libinput drivers) give pressure, tilt and the eraser through XInput2, and Wayland tablets work with `RUSTY_PAINTER_ENABLE_WAYLAND_TABLET=1`.
- **Layers**: Full layer support with visibility, opacity, blending, folders, masks, clipping masks (Ctrl+Alt+G: a layer shows only where the one below has paint) and adjustment layers (Filter → New Adjustment Layer: brightness/contrast, hue/saturation, levels, curves, colour balance, gradient map, exposure, temperature, vibrance, sepia, solarize, invert, desaturate, posterize or threshold over everything below, live). Merge Down (Ctrl+Alt+E), Merge Visible (Ctrl+Shift+E) and Flatten Image, as shown on screen. Vector layers (Layer → New Vector Layer): the brush draws lines that stay lines, smoothed and pressure-sensitive; the eraser takes out whole lines, only the part it touches, or the stretch up to where other lines cross it; Edit Lines (Layer → Vector) bends a line by its handles, widens it around one or moves it; Layer → Vector thickens or thins every line, recolours them or rasterises the layer; any other pixel tool turns it into a plain layer (undo brings the lines back). Fill layers (Layer → New Fill Layer: a colour or a gradient, linear, radial, reflected or angle, any colours; its mask or clipping says where it shows) and a border around any layer's paint (Layer → Border…: width, colour, opacity; it follows the paint as you draw and becomes pixels when merged). Lock a layer's position; mark it as a draft (shown, but left out of export and merging) or as a reference for fills, the magic wand and colour range.
- **Filters**: Brightness/contrast, hue/saturation, levels, curves (a master curve and one per channel), colour balance (shadows, midtones, highlights), gradient map (any colours, with presets), invert, desaturate, posterize, threshold, exposure, temperature/tint, vibrance, sepia, solarize, Gaussian, motion, zoom and spin blur, sharpen, reduce noise (median), glow, chromatic aberration, halftone, emboss, find edges, oil paint, vignette, noise, pixelate, dither, clouds, and Extract Line Art (a scanned drawing's paper turns transparent), previewed live on the layer or the selection. Sliders preview smoothly even on a whole 4K canvas.
- **Image menu**: canvas size (with an anchor), image size (smooth or hard pixels), crop to selection, rotate and flip the whole document, each one undo step.
- **Text**: the Text tool types onto a text layer in any shipped or system font, with size, alignment, spacing and colour; drag it into place before keeping it. The text stays editable: click it with the Text tool or double-click the layer. Painting on it turns it into pixels (Layer → Rasterise Text does it on purpose).
- **Shader layers**: the **fx** button in the layers panel adds a layer drawn by a GLSL shader, written Shadertoy style (`mainImage(out vec4 fragColor, in vec2 fragCoord)` with `iTime`, `iResolution`, `iMouse`, `iFrame`, and `texture(iChannel0, uv)` reading the layers below it). It animates live on the GPU, under and over your painting, with the layer's blend mode and opacity. Each shader layer gets its own editor window (double-click the layer): syntax colouring, errors at their line as you type (the canvas keeps the last version that worked), play/pause, speed and templates (plasma, rings, clouds, stars, ripple, chromatic split, glow, vignette). Export, merging and saving use the current frame. It plays live unless the shader layer is in a folder, has a mask, clipping or a border, or has an adjustment layer above it; then it shows as a still frame updated every second.
- **QuickShape**: hold the pen still at the end of a stroke and it becomes a clean line, ellipse (turned the way it was drawn), rectangle or polygon, editable before it's applied.
- **Animation**: frame by frame. View → Timeline opens the timeline under the canvas; Animate layer makes the selected layer animated (its picture the first drawing). Each animated layer has drawings that start on a frame and are held until the next; add one blank or as a copy, drag it along, take it away (each one undo step; a stroke undoes on its own drawing whatever frame shows). Play at any frame rate over a range of frames, step with `,` and `.` (Shift+Space plays), and see onion skins of the drawings before (red) and after (green). File → Export Animation writes a GIF, an animated PNG, MP4 or WebM (with ffmpeg); saved documents keep their timeline.
- **Skeletal animation**: File → Import Animation brings in Spine (JSON with its atlas or loose pictures), DragonBones (JSON with its texture atlas) and Lottie animations as rig layers: bones, slots, region and weighted mesh attachments, IK, and animated bones, colours, attachment swaps and mesh deforms, posed at the timeline's frame and redrawn as it plays. The timeline's rig section picks the animation and turns or moves bones, keyed at the frame showing. File → Import Video as Frames brings a video (through ffmpeg), GIF, animated PNG or WebP in as an animated layer, a drawing a frame: the way in for Moho or Alight Motion work, through what they export.
- **Time-lapse**: File → Record Time-lapse keeps a frame after each change; export it as MP4 (with `ffmpeg` installed) or GIF.
- **Autosave**: unsaved work is kept once the app is idle, and offered back after a crash or a quit without saving.
- **Selection Tools**: Rectangle, ellipse, lasso (with smoothing), polygon, magnetic lasso, magic wand, colour range and selection brush; replace, add, erase and intersect; invert; every change undoable. The Transform tool moves, scales, rotates, puts in perspective or distorts the selection outline itself when there's nothing under it. Select → Modify grows, shrinks, feathers or borders the selection; Ctrl+click a layer's thumbnail selects its paint (Shift adds, Alt subtracts); selections can be saved by name with the project and loaded back; Quick Mask (Shift+Q) paints the selection with the brush and eraser.
- **Viewing Aids**: View → Grid (square or isometric, with a pixel grid from 800%), guides (Ctrl+drag from beside the canvas, or View → Guides) with Snap to Guides / Snap to Grid, a Reference Image window to pick colours from, and a Navigator showing the whole canvas.
- **Wrap Around**: View → Wrap Around paints across the canvas's edges and shows it repeated, for seamless tiles.
- **Mirror Painting**: Left/right, top/bottom, four-way or radial (mandala, optionally kaleidoscope) around a centre and angle you drag on the canvas; flip the view with H.
- **Shapes, Ruler & Assistants**: Line, rectangle, ellipse, polygon and Bézier curve (click for corners, drag for smooth points, drag the anchors and handles afterwards; Alt breaks a handle pair) drawn with the brush, filled, or both, editable until applied; a ruler that straightens strokes along or parallel to it; vanishing-point, perspective, ellipse and concentric assistants strokes snap to (View → Assistants, or the Shape menu). Guides are saved with the project.
- **Gradients**: Linear, radial, reflected and angle gradients, previewed live, dithered, clipped to the selection.
- **Smart Patch**: Content-aware fill of the selection from its surroundings (PatchMatch), or paint over something with the selection brush to remove it.
- **Transform Tools**: Move, rotate, scale and flip selections or layers with a live preview; **Perspective** (drag the corners of a plane seen at an angle) and **Distort** (a grid of points, 1×1 to 5×5 cells, inner points included: the picture bends smoothly through them). The selection follows what was transformed.
- **History**: Edit → History (`Ctrl+H`) lists every step by what it did (the tool, filter or command); click one to go back to it, or forward again. One undo history for the whole document (pixels, selections, transforms, layer add/remove/reorder/merge, canvas resizes; all of it kept in saved files), undone in the order things were done whichever layer is selected.
- **Canvas**: Large canvas support (default 4000x4000) backed by tiled storage and GPU texture atlases.
- **Project Files**: Save/open your work as a `.rpainter` project (compressed, includes undo history) via the Open/Save buttons in the top bar. File managers show a preview of the drawing, and Krita, GIMP or MyPaint can open it as a flattened image.
- **Colour depth**: a document keeps 8 bits per channel, 16 bits, or 32-bit floats in linear light (pick it for a new canvas, or Image → Colour Depth). Brush strokes, gradients, colour adjustments and the Image menu's operations work at the document's full depth, so soft airbrushing and gradients don't band and faint glazes build up; undo, saved files and 16-bit/float exports keep it. Other tools still work at 8 bits on the tiles they change.
- **Colour management**: a document has a colour profile (sRGB, Display P3, Adobe RGB, Rec. 2020 or any RGB ICC profile): Image → Colour Profile assigns or converts it (with a rendering intent), the canvas is shown converted for your monitor's profile (View → Colour Management), and View → Colour Management → Proof Colours shows how it would print on a CMYK profile (yours, or one found on the system), with a gamut warning. Exports embed the profile (PNG, JPEG, WebP, TIFF, PSD), pictures and Photoshop/Krita files come in with theirs, and TIFF (CMYK) writes print-ready CMYK. Live shader layers show unconverted. The colour picker shows colour harmonies (complementary, split, analogous, triadic, tetradic) on its wheel.
- **Export**: Save your work as PNG (8 or 16-bit), JPEG, TIFF (8, 16-bit or 32-bit float), lossless WebP, a layered PSD (16-bit for a deeper document), or a layered SVG (vector layers as editable paths, fill layers as shapes, paint layers as embedded pictures). Photoshop files (`.psd`, 8 or 16-bit RGB or grayscale) open with their layers, folders, masks, blend modes and clipping. Krita documents (`.kra`, 8 or 16-bit or float RGBA, opened at their depth) open with their paint layers, folders, transparency masks, opacity, visibility and blend modes, and Clip Studio Paint documents (`.clip`) with their raster layers, folders, masks, paper colour, clipping and blend modes; anything that can't come across (filter layers, vector data) is left out and the app's own saved picture of the document is added as a hidden layer to compare against.
- **Performance**: Optional masked brush mode and zoom-out LOD for performance experiments.
- **Android** (experimental): Runs on Android via a patched `winit` and native activity glue (see [Android (APK build)](#android-apk-build) below).

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

### Windows
Build with the MSVC Rust toolchain (Visual Studio's C++ build tools and the Windows SDK); `./scripts/check-windows.ps1` runs the same checks as CI. Release builds have no console, so the app logs to `rusty-painter.log` in the data folder above (errors at start-up, the GPU it picked, panics with their backtrace). See [docs/windows-debugging.md](docs/windows-debugging.md) for what to try when it doesn't start or the pen misbehaves.

## Android (APK build)
This is an experimental setup and may need platform fixes. Building for Android needs more than the target and `cargo-apk`: this repo vendors and patches `winit` and ships its own NDK linker wrappers, both required for the build to work:

- `vendor-winit/`: a patched copy of `winit`, pulled in via `[patch.crates-io]` in `Cargo.toml`, needed for Android windowing/lifecycle support beyond what upstream `winit` provides out of the box.
- `scripts/linkers/` + `scripts/build-android.sh`: wrapper scripts that route each Android target's linker invocation through the NDK toolchain; `.cargo/config.toml` points cargo at them. Run `scripts/build-android.sh` rather than a bare `cargo apk build` to make sure these are picked up.
- `src/android.rs`: the native JNI/lifecycle glue (`android_logger`, `jni`, `ndk-context`) that makes the app actually run once launched, as opposed to just compiling for the target.
- `android/java/` + `android/classes.dex`: two small Java helpers the NativeActivity can't do without: the system file picker (open/save documents, brushes, palettes) and the soft keyboard. `cargo-apk` packs no Java, so the dex is committed, embedded in the library and loaded at run time; after editing the Java, rebuild it with `scripts/build-android-dex.sh` (needs a JDK, e.g. Android Studio's `jbr`).

On Android the app starts in the project library (projects live in the app's storage, in folders); exports go to `Pictures/Rusty Painter` (images) or `Download/Rusty Painter` (PSD, SVG), time-lapses to Pictures as GIF (there's no ffmpeg on Android).

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
- **CI** (`.github/workflows/ci.yml`) runs on every push to `master` and on pull requests: `cargo fmt --check`, `clippy -D warnings`, tests, a bench build and a docs build on Linux and Windows, plus an Android compile check and `cargo audit`.
- **Release** (`.github/workflows/release.yml`) runs the same checks first, then builds and publishes:
  - **By hand:** GitHub → *Actions* → *Release* → *Run workflow*. Enter the version and tick the platforms (Linux, Windows, Android). Untick *Publish* to only build; the files are then downloadable from the run page.
  - **By tag:** `git tag v0.2.0 && git push origin v0.2.0` builds all three and publishes release `v0.2.0`.
  - Assets: `rusty-painter-<version>-linux-x86_64.tar.gz`, `-windows-x86_64.zip`, `-android-arm64.apk`, plus a source zip.
- **Android signing in CI:** add two repository secrets (*Settings → Secrets and variables → Actions*):
  - `ANDROID_KEYSTORE_BASE64`: `base64 -w0 release.keystore`
  - `ANDROID_KEYSTORE_PASSWORD`: its password

  Without them the APK is signed with a throwaway key: it installs, but Android won't let a later release update it (different signature), so set them before sharing APKs. Keep the keystore safe: losing it means users must uninstall to upgrade.

## Controls
The full list is in **Help → Keyboard Shortcuts**, labelled for your keyboard (QWERTY, AZERTY or QWERTZ, detected or set in Settings): letter shortcuts follow the letter, digit and symbol ones the key's position. Click any shortcut there and press new keys to change it (a key already in use moves over, and the window says from which command); changes are kept in `settings.json`. The defaults (QWERTY names):

- **Paint**: left drag (pen pressure where supported). `B` brush, `E` eraser, `[` / `]` size, `K` (hold) or right-click the pop-up brush palette.
- **View**: `Space` + drag or right drag to pan, middle drag to rotate, wheel to zoom, `Ctrl+0` fit, `Ctrl+1` actual pixels, `H` flip, `Ctrl+'` grid, `Ctrl+;` guides (Ctrl+drag a guide to move it). Two fingers pan, pinch and twist.
- **Tools**: `M` rectangle/ellipse select, `L` lasso (again: polygon, magnetic), `Q` magic wand, `Shift+Q` quick mask, `U` shapes, `Shift+G` gradient, `V`/`T` transform, `I` eyedropper, `G` fill (again: enclose, lasso delete), `W` liquify, `S` smudge/blur (smudge also deforms or clones: Ctrl+click the clone source; blur also sharpens or adjusts colour), `R` ruler (assistants: View → Assistants).
- **Edit**: `Ctrl+Z` undo, `Ctrl+Shift+Z` / `Ctrl+Y` redo (one history for the whole document), `Ctrl+X` / `Ctrl+C` / `Ctrl+V` cut / copy / paste (pastes as a new layer, ready to transform; images from other programs too), `Ctrl+Shift+C` copy merged, `Ctrl+J` duplicate layer, `Ctrl+Alt+E` merge down, `Ctrl+Shift+E` merge visible, `Ctrl+A` select all, `Ctrl+D` / `Esc` deselect, `Ctrl+Shift+I` invert, `Delete` erase the selected pixels, `Shift+F5` content-aware fill, `Enter` / `Esc` apply / cancel a transform.
- **File**: `Ctrl+N` new, `Ctrl+O` open, `Ctrl+S` save, `Ctrl+E` export, `Ctrl+Shift+O` import an image as a layer, or drop image files on the window (on the Palette window, a dropped picture gives its colours instead).

## Project Files
Work is saved as a single `.rpainter` file via **Open**/**Save** in the top bar: layers, tile data, and undo history all round-trip. The file is an [OpenRaster](https://www.openraster.org/) archive: a ZIP with `mergedimage.png` (the flattened picture) and `Thumbnails/thumbnail.png`, which is what file managers such as Dolphin use for the preview. The project itself is one more entry, `rusty-painter/project.rpnt`, in a versioned binary format (`src/project/`): tile pixel data is zstd-compressed per tile, and layers are matched up by a stable id (not position) so undo stays correct even if you'd reordered layers before saving. Older project files (including the bare binary files saved before the OpenRaster container) remain loadable after format additions; new fields default sensibly on read rather than breaking the load.

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
├── project/             .rpainter save/open, image export, .psd/.kra/.clip opening
├── tablet/              pen input (octotablet: Windows Ink, Wayland)
├── ui/                  egui panels, menus, tool options, theme (style.rs tokens, theme.rs)
├── android.rs           Android platform glue
└── bench_api.rs         headless entry points for benches/app_bench.rs (feature `bench`)
```

The [developer guide](docs/wiki/Developer-Guide.md) says where a typical change goes and how to test it; [architecture](docs/wiki/Architecture.md) covers the data model and pipelines; [performance](docs/performance.md) has timings for every filter, layer kind, export and import on a 4000 px canvas, and what the quality tests check.

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
