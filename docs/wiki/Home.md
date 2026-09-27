# Rusty Painter wiki

Rusty Painter is a painting app written in Rust on egui and wgpu. Its canvas is split into tiles, brushes are painted on a separate thread, and it runs on Linux, Windows and Android.

- [Architecture](Architecture.md): how the code is laid out, the data model, and how a frame, a stroke, an undo and a save move through it.
- [Developer guide](Developer-Guide.md): building, checks, where each kind of change goes, adding a tool from start to finish, tests and benchmarks.

For user-facing features, controls and building for Android, see the [README](../../README.md).

These pages live in `docs/wiki/` so they are reviewed together with the code. They use plain Markdown links, so the folder can also be pushed as the repository's GitHub wiki.
