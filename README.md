# Rusty Painter

A lightweight desktop painting playground built with Rust and `eframe/egui`. The goal of Rusty Painter is to explore fast 2D painting techniques (tiling, atlases, multithreaded brushes) while keeping the UI simple.

## Features
- Soft, hard, and pixel brushes with spacing, flow, jitter, and stabilizer options
- Brush presets with a live color picker
- Layer support with per-layer visibility and opacity
- Undo/redo history
- Massive canvas (default 8000x8000) backed by tiled storage and GPU texture atlases
- Optional masked brush mode and zoom-out LOD for performance experiments
- Optional GPU backend (`--gpu`) that renders directly with wgpu shaders (see `src/shaders/`)

## Quick Start
Prerequisites: Rust toolchain (`cargo`, `rustc`) installed.

```bash
cargo run            # CPU/egui path (default)
cargo run -- --gpu   # GPU path (wgpu backend)
```

That launches the native egui window with the default canvas and brush settings.

## Controls
- Paint: Left click and drag
- Pan: Hold `Space` + left drag
- Zoom: Middle-click drag vertically
- Rotate: Right-click drag horizontally
- Clear canvas: `C`
- Undo/redo: `Ctrl+Z` / `Ctrl+Shift+Z`

## UI Panels
- **Brush Settings**: Choose brush type/mode, size, hardness, flow, spacing, jitter, stabilizer, pixel-perfect mode, AA.
- **Color Picker**: Triangle HSVA picker with opacity slider.
- **Brush Presets**: Quick presets; selecting one keeps your current color.
- **Layers**: Add/remove layers, toggle visibility, set opacity, choose active layer.
- **General Settings**: Toggle masked brush (fast), high-quality zoom out (slower), adjust brush thread count.

## Project Structure
- `src/main.rs` – egui app wiring, input handling, texture atlas uploads.
- `src/canvas/` – tiled canvas storage, compositing, and undo history.
- `src/brush_engine/` – brush logic, stroke spacing, and mask generation.
- `src/ui/` – egui panels for brushes, colors, layers, and settings.
- `src/utils/` – small helpers (colors, vectors, profiling).

## Contributing
The project is early-stage and focused on performance experiments. If you have ideas for improving brush quality, tiling performance, or UI/UX, feel free to open an issue or directly contact me. Tests/benchmarks and profiling notes are especially welcome.
