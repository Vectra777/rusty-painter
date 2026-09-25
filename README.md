# Rusty Painter

A lightweight linux desktop painting playground built with Rust and `eframe/egui`. The goal of Rusty Painter is to explore fast 2D painting techniques (tiling, atlases, multithreaded brushes) while keeping the UI simple.

![](/imgs/2025-12-1118-06-17-ezgif.com-video-to-gif-converter.gif)

## Features
- **Brush Engine**: Soft, hard, and pixel brushes with spacing, flow, jitter, and stabilizer options.
- **Tablet Support**: Pressure sensitivity and eraser support via `octotablet`.
- **Layers**: Full layer support with visibility, opacity, and blending.
- **Selection Tools**: Rectangle, Circle, and Lasso selection modes.
- **Transform Tools**: Move, rotate, and scale selections with non-destructive preview.
- **History**: Robust Undo/redo system for pixels, selections, transformations, and layer add/remove/reorder/merge.
- **Canvas**: Large canvas support (default 4000x4000) backed by tiled storage and GPU texture atlases.
- **Project Files**: Save/open your work as a `.rpainter` project (compressed, includes undo history) via the Open/Save buttons in the top bar.
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
- `src/utils/android.rs` — the native JNI/lifecycle glue (`android_logger`, `jni`, `ndk-context`) that makes the app actually run once launched, as opposed to just compiling for the target.

To build:

```bash
rustup target add aarch64-linux-android
cargo install cargo-apk
scripts/build-android.sh
```

If you have multiple Android SDK/NDK installs, ensure your environment points to the intended one (e.g. `ANDROID_HOME` and `ANDROID_NDK_HOME`).

**Release signing** is not committed. `Cargo.toml` has a commented-out `[package.metadata.android.signing.release]` template; `cargo-apk` reads signing config as plain TOML with no environment-variable interpolation, so uncomment and fill it in locally (never commit it), or sign the built APK afterwards with `apksigner` instead.

## Controls
- **Paint**: Left click and drag
- **Pan**: Hold `Space` + left drag
- **Zoom**: Middle-click drag vertically
- **Rotate Canvas**: Right-click drag horizontally
- **Clear Canvas**: `C`
- **Undo**: `Ctrl+Z`
- **Redo**: `Ctrl+Shift+Z`
- **Cancel Selection**: `Escape`
- **Commit Transform**: `Enter`

## Project Files
Work is saved as a single `.rpainter` file via **Open**/**Save** in the top bar — layers, tile data, and undo history all round-trip. Internally it's a versioned binary format (`src/project/`): tile pixel data is zstd-compressed per tile, layers are matched up by a stable id (not position) so undo stays correct even if you'd reordered layers before saving, and the thumbnail preview is stored uncompressed since it's already PNG-encoded. Older project files remain loadable after format additions — new fields default sensibly on read rather than breaking the load.

## UI Panels
- **Top Bar**: Switch between Brush, Select (Rect, Circle, Lasso), and Transform tools.
- **Brush Settings**: Choose brush type/mode, size, hardness, flow, spacing, jitter, stabilizer, pixel-perfect mode, AA.
- **Color Picker**: Triangle HSVA picker with opacity slider.
- **Brush Presets**: Quick presets; selecting one keeps your current color.
- **Layers**: Add/remove layers, drag to reorder, toggle visibility, set opacity, choose active layer.
- **General Settings**: Toggle masked brush (fast), high-quality zoom out (slower), adjust brush thread count.
- **Export**: Export your canvas via the Export button in the top bar.

## Project Structure
- `src/main.rs` – native app entry point.
- `src/app/` - Application state, input handling, and tool logic.
- `src/canvas/` – tiled canvas storage, compositing, and undo history.
- `src/brush_engine/` – brush logic, stroke spacing, and mask generation.
- `src/selection/` - Selection shapes and transformation logic.
- `src/tablet/` - Tablet input handling.
- `src/ui/` – egui panels for brushes, colors, layers, and settings.
- `src/utils/` – small helpers for colors and exporting.
- `src/project/` – `.rpainter` project file save/load format.

## Contributing
The project is early-stage and focused on performance experiments. If you have ideas for improving brush quality, tiling performance, or UI/UX, feel free to open an issue or directly contact me. Tests and benchmarks are especially welcome.
