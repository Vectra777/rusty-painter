# Rusty Painter

A lightweight linux desktop painting playground built with Rust and `eframe/egui`. The goal of Rusty Painter is to explore fast 2D painting techniques (tiling, atlases, multithreaded brushes) while keeping the UI simple.

## Features
- **Brush Engine**: Soft, hard, and pixel brushes with spacing, flow, jitter, and stabilizer options.
- **Tablet Support**: Pressure sensitivity and eraser support via `octotablet`.
- **Layers**: Full layer support with visibility, opacity, and blending.
- **Selection Tools**: Rectangle, Circle, and Lasso selection modes.
- **Transform Tools**: Move, rotate, and scale selections with non-destructive preview.
- **History**: Robust Undo/redo system for pixels, selections, and transformations.
- **Canvas**: Massive canvas support (default 8000x8000) backed by tiled storage and GPU texture atlases.
- **Export**: Save your work as PNG, JPEG, or TIFF.
- **Performance**: Optional masked brush mode and zoom-out LOD for performance experiments.

## Quick Start
Prerequisites: Rust toolchain (`cargo`, `rustc`) installed.

```bash
cargo run --release
```

That launches the native egui window with the default canvas and brush settings.

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

## UI Panels
- **Top Bar**: Switch between Brush, Select (Rect, Circle, Lasso), and Transform tools.
- **Brush Settings**: Choose brush type/mode, size, hardness, flow, spacing, jitter, stabilizer, pixel-perfect mode, AA.
- **Color Picker**: Triangle HSVA picker with opacity slider.
- **Brush Presets**: Quick presets; selecting one keeps your current color.
- **Layers**: Add/remove layers, toggle visibility, set opacity, choose active layer.
- **General Settings**: Toggle masked brush (fast), high-quality zoom out (slower), adjust brush thread count.
- **Export**: Export your canvas via the Export button in the top bar.

## Project Structure
- `src/main.rs` – egui app wiring, input handling, texture atlas uploads.
- `src/app/` - Application state, input handling, and tool logic.
- `src/canvas/` – tiled canvas storage, compositing, and undo history.
- `src/brush_engine/` – brush logic, stroke spacing, and mask generation.
- `src/selection/` - Selection shapes and transformation logic.
- `src/tablet/` - Tablet input handling.
- `src/ui/` – egui panels for brushes, colors, layers, and settings.
- `src/utils/` – small helpers (colors, vectors, profiling, exporting).

## Contributing
The project is early-stage and focused on performance experiments. If you have ideas for improving brush quality, tiling performance, or UI/UX, feel free to open an issue or directly contact me. Tests/benchmarks and profiling notes are especially welcome.
