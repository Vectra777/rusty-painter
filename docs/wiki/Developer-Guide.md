# Developer guide

## Build and run

```bash
cargo run --release                     # the desktop app
scripts/build-android.sh                # an APK (see the README for NDK setup and signing)
```

## Checks

CI runs these on every push (`.github/workflows/ci.yml`). Run them before sending a change:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo check --locked --benches --features bench
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items
```

CI also compiles the library for `aarch64-linux-android`. Android-only code, such as `src/android.rs` and the vendored winit, isn't built on desktop, so run that check too when you touch it.

## Where things go

| To change... | Look in |
| --- | --- |
| What a tool does on press, drag or release | `src/app/tools/<tool>.rs` |
| Which tool gets an input event | `src/app/input/mod.rs`: `handle_primary_press`, `handle_pen_drag`, `handle_tool_move`, `handle_primary_release` |
| Touch gestures, palm rejection, pen pressure | `src/app/input/touch.rs`, `src/tablet/` |
| A keyboard shortcut | `src/app/input/shortcuts.rs`, and its row in `SHORTCUTS` in `src/ui/general_settings.rs` (the help window) |
| A tool's options bar | `src/ui/tool_options.rs` (`options_row` picks the row for each tool) |
| Menus | `src/ui/menus.rs` |
| Toolbar buttons and icons | `src/ui/toolbar.rs`, `src/ui/icons.rs` |
| Touch controls on the canvas | `src/ui/canvas_sliders.rs` |
| Colours, spacing, sizes | `src/ui/style.rs` (tokens); `src/ui/theme.rs` applies them to egui |
| Brush rendering | `src/brush_engine/` (`brush.rs` for tips and dabs, `stroke.rs` for spacing and pressure; one module per engine or feature: `bristle`, `sketch`, `hatching`, `dual`, `wet_edge`) |
| Brush presets and importing | `src/brush_engine/preset_file.rs` (`.rpbrush`), `src/brush_engine/import/` (one module per app), `src/app/brush_io.rs` (the user's library) |
| Smudge, blur, deform, clone | `src/app/tools/blend.rs` |
| Ruler and assistants | `src/app/tools/guides.rs`, `src/app/tools/assistants.rs` |
| A pixel algorithm (fill, gradient, inpaint...) | `src/canvas/<algorithm>.rs`, as a pure function |
| How layers are blended | `src/canvas/storage/composite.rs`, `src/canvas/blend_modes.rs` |
| Writing pixels with undo | `src/canvas/storage/pixels.rs` (`paint_mask`, `paint_region`, `map_layer_pixels`, `write_layer_region`) |
| Layer operations | `src/app/canvas_ops.rs` (keeps per-layer histories in step) |
| The project file format | `src/project/mod.rs`, `src/project/convert.rs` |

## Adding a tool, step by step

Take a "Stamp" tool that paints something where you click:

1. **The algorithm.**
   - If the tool computes pixels, write that as a pure function in `src/canvas/`, with unit tests beside it.
   - Use a `Canvas` pixel writer (`paint_region` or `paint_mask`) to put the result on the layer. The writer records undo in the `UndoAction` you pass it.
2. **The tool.**
   - Add `Tool::Stamp` in `src/app/tools/mod.rs`.
   - Create `src/app/tools/stamp.rs` with `impl PainterApp { fn stamp_press(&mut self, pos) ... }`.
   - Push one `UndoAction` per user action onto `self.layer_state.histories[active]`.
   - Mark the changed tiles for redraw with `mark_rect_damage` or `mark_tiles_in_bounds_dirty`.
3. **Input.**
   - Add the tool to the matches in `src/app/input/mod.rs`: press, pen drag, move and release. The compiler lists every match you missed.
4. **A session, if the tool has one.**
   - This is for state that lasts between presses, like a shape still being edited.
   - Keep it in `WorkspaceState` (`src/app/state.rs`).
   - End it in `settle_tool_sessions` (`src/app/painter.rs`) when the tool is left.
   - End it in `end_tool_sessions` (`src/app/canvas_ops.rs`) when the document is replaced.
5. **UI.**
   - Add a toolbar button and icon.
   - Add an options row in `tool_options.rs`, a shortcut plus its help row, and touch controls in `canvas_sliders.rs` if the tool needs them.
6. **Tests.**
   - App-level tests build a headless app with `crate::project::tests::test_app_pub(canvas)`, call the tool's methods, and check the pixels and the undo depth. `app/tools/*.rs` has many examples.
   - Check that one action gives exactly one undo step and that undo restores the pixels.
7. **Benchmark.**
   - Add an entry point to `src/bench_api.rs` and a case to `benches/app_bench.rs`.
   - Run it before and after optimising (see below).

## Tests

- Unit tests sit next to the code (`#[cfg(test)] mod tests`). The canvas storage tests are in `src/canvas/storage/tests.rs`.
- App-level tests use `project::tests::test_app_pub`, which gives a real `PainterApp` with a stroke worker and no window.
- Slow timing tests are `#[ignore]`d. Run them with `cargo test --release -- --ignored --nocapture`.

## Benchmarks

```bash
cargo bench --features bench --bench app_bench            # every tool end to end on a painted 4000 px canvas
cargo bench --bench tools_bench                           # tool engines on their own
cargo bench --bench brush_bench                           # brush stamping and compositing
cargo bench --bench tools_bench -- --save-baseline before
cargo bench --bench tools_bench -- --baseline before      # compare against a saved run
```

The full `app_bench` run takes a while, so start it in a spare terminal.

## Conventions

- **Undo.** One user action is one undo step. Pixel writers take a `&mut UndoAction`; the tool pushes it once.
- **Canvas access.** Get mutable access only through `canvas_mut()` or `release_canvas()` followed by `exclusive`. Never hold the canvas across a stroke.
- **Layers.** Refer to a layer by `LayerId` for anything that lasts beyond a frame.
- **Tool methods.** A tool's methods live on `PainterApp`, in that tool's module, named `<tool>_press`, `<tool>_drag` or `_move`, `<tool>_release` and `<tool>_commit`.
- **Commits.** Messages are a short sentence saying what changed for the user or the code.
