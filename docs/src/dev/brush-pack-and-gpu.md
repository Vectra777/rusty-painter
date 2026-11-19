# Brush packs, folders, and GPU operations

## Default brush pack

The default brushes are the app's own presets plus a pack: one `.rpbrush`
file holding 96 brushes (each shape and grain stored once) and a
`manifest.json` listing its collections (Sketching, Inking, Painting,
Textures, Effects) and their brushes, in order. The 96 were chosen from a
larger converted set as the ones that paint fully and quickly (most under
half a second for a long stroke), a few of each kind. It lives in
`assets/brush-pack/default-v1`, the `.rpbrush` in Git LFS (`.gitattributes`):
a clone without LFS has only a small pointer there, which nothing but the
release builds reads. It isn't compiled in; the Linux, Windows and Android
release jobs pull it (through the Actions cache, keyed by its LFS pointers,
so an unchanged pack isn't downloaded from LFS again) and put it in every
build. The app looks for it, in order:

1. in the data folder, `brushes/packs/default-v1` (Linux's `install.sh`
   copies it there);
2. in a `brushes/default-v1` folder beside the program (the Windows and
   Linux archives);
3. on Android, in the APK's assets (`target/android-assets`, see
   `Cargo.toml`), copied into the data folder on the first start (and again
   when an update brings a different pack).

The pack's grains are rounded to 8-bit steps (the precision of the pictures
they came from) and compressed harder than the app's own saves (zstd 19
rather than 9; shapes as maximally compressed PNG). That makes the pack
96 MB; the app reads it like any `.rpbrush`.

At the first launch that finds the pack, its collections are copied into
editable collections in the brush library, all under one **Default
brushes** folder with a subfolder per collection, and each brush is tagged
by its collection (once; tags the user removes stay removed). The app's own
built-in presets join them there, each in its kind's folder by its first
tag (`default_folder`), with **Erasers** and **Pixel** folders of their
own. Custom presets, tags, favorites, recent brushes and saved tools are
kept. Deleted defaults stay deleted until **Restore default brushes**;
resetting a pack brush reads its settings from the pack again.

Brushes are named by category and character (for example "Watercolor Wet
Bloom", "Pencil Tapered Soft Fine"), numbered where two would match.
Grains are named by how they look: kind (Paper, Canvas, Noise, Speckle,
Streaks, Cloud, Blotch, Pattern, Plain), scale (Fine, Medium, Coarse) and
tone (Light, Dark), such as "Paper Medium Dark 4". Identical grains share a
name. The five built-in grains keep their names.

## The figure pack

The realistic 3D figures (male, female, child, left and right hand) are a
second pack, `assets/figure-pack/v1`: a `.glb` per model (Git LFS, about
3 MB in all), a `manifest.json` listing them (id, name, file, kind, licence,
source) with `content`, the CRC-32 of the model files one after the other
in the manifest's order (a test keeps it in step), and `LICENSE.md`.
`src/canvas/figure/pack.rs` finds it as the brush pack is found: the data
folder's `figures/v1` (Android unpacks the APK's `figures-v1/` there first
when its manifest or a file's size changed), then `figures/v1` beside the
program, then, in debug builds, the repository's. A checkout without LFS
has only pointers there: the app then behaves as if the pack were missing
(the mannequin alone), and the tests that need the models skip. Releases
copy it beside the program (`release.yml`), into the APK
(`build-android.sh`, `release.yml`), and `install.sh` into the data folder.

The models are made, reproducibly, from MakeHuman's CC0 assets:
`scripts/make_figure_pack.sh` clones MPFB 2 at a fixed commit, installs it
into a throwaway Blender profile and runs `scripts/make_figure_pack.py` in
Blender, which builds each figure from MakeHuman's macro sliders, rigs it
with MPFB's `game_engine` skeleton (Unreal names, 53 joints with three in
each finger), re-bases it from MakeHuman's rest pose to the T-pose the app
assumes (arms level, palms down, fingers straight, the thumb as the
mannequin holds it), makes the body-shape morph targets from the same
sliders moved (build only, so the skeleton still fits), and writes the
`.glb` files and the manifest. Each body is MakeHuman's base mesh as it is
(about 13,500 vertices, 27,000 triangles, no decimation needed); picking
one with the pointer takes about 0.2 ms on the CPU.

## The hand and body tracking models

The Hand Scanner's and Pose from Camera / Photo's models are a third pack,
`assets/pose-models/v1`: MediaPipe's palm detector and hand landmark model
(Apache-2.0, February 2023) and its BlazePose pose detector and full-body
landmark model (Apache-2.0, March 2023), all as ONNX from OpenCV's model
zoo (`palm_detection.onnx`, 3.7 MB, `hand_landmark.onnx`, 3.9 MB,
`pose_detection.onnx`, 12 MB, `pose_landmark.onnx`, 5.3 MB; Git LFS), a
`manifest.json` (file, licence,
source URL and SHA-256 of each, and `content`, their CRC-32, kept in step
by a test) and the Apache `LICENSE`. `src/pose_track/mod.rs` finds them as
the figure pack is found (the data folder's `pose-models/v1`, Android's
APK `pose-models-v1/` unpacked there first; `pose-models/v1` beside the
program; the repository's in debug builds) and loads them with tract once,
when the camera window (or a photo) first needs them. The releases ship them as they ship the
figures. Without them the camera window says the models aren't installed,
and the tests that run them skip.

Body tracking (`src/pose_track/body.rs`) runs the detector on the whole
picture (224 px, letterboxed), crops a square round the hips turned
upright from its alignment points, and reads 33 landmarks (pixels, metres
round the hips, visibility) from it at 256 px; the next frame's crop comes
from the model's own alignment points, so the detector runs only when the
body is lost. With hands asked for, each wrist's crop (from the wrist and
knuckle landmarks) goes to the hand landmark model, then follows the hand
frame to frame. `canvas::figure::retarget::retarget_body` turns the metric
landmarks into canonical joint turns. On this desktop (release build, one
thread) the detector takes about 22 ms and the landmarks 27 ms; each hand
16 ms more.

All of it (the models, `src/pose_track`, the camera in `src/capture`) is
behind the cargo feature `pose-capture`, on by default. The desktop camera
is nokhwa's (V4L2 on Linux, whose bindings need libclang to build; Media
Foundation on Windows); Android's is `CameraFeed.java` (Camera2), and
macOS and iOS have no camera yet.

## Organizing brushes

The presets window uses a single scrollable column. A dropdown at the top
selects a root folder, Favorites, all brushes, or recent brushes. Subfolders
appear as folder rows alongside the brushes; click one to enter it and use the
up button to return to its parent. The window can shrink vertically without the
list pushing it back to its previous height.

Create folders through **Manage folders**. Drag a brush tile or its dotted grip
to reorder it or drop it into a subfolder; on touch, hold the grip first. Context
menus provide folder rename, move, and delete actions. Deleting a folder keeps
its brushes and subfolders. Exports retain each brush's folder path.

Each brush has a star at the top left. Starred brushes appear before other
entries in their own folder, without changing their stored manual order.
**Favorites** references the same brushes across the entire library: choosing or
editing a favorite acts on its original preset, and removing its star removes
only the favorite reference.

Source categories seed appropriate default tags (Pencil, Inking, Watercolor,
Texture, Light, and so on) once, without overwriting custom tags. Removed tags
stay removed after restarting. Search and the tag dropdown filter within the
current folder and its descendants; navigating folders clears the tag filter.

`library.json` version 2 assigns persistent IDs for folder membership and order,
while maintaining compatibility with the existing name-based tags and tool
slots. Preset names remain unique; collisions receive suffixes. Writes use the
existing atomic file writer. Multi-preset collection edits retain the other
brushes and load resources before changing archive offsets.

## GPU implementation and measurements

The shared wgpu compute service supports brightness/contrast, levels, curves,
exposure, invert, desaturation, hue/saturation, color balance, gradient map,
Gaussian blur, and sharpen. Each kind of pass (per-pixel filters, blur
input, row blur, column blur, blur finish) is its own shader entry point
over one binding layout. Separable blur uses the same three-box
approximation and intermediate rounding as the CPU reference. The row pass
reads each row segment once into workgroup memory; the column pass keeps a
running window sum, so its cost doesn't grow with the radius.

Images go in bands of whole rows (up to 16 MB each), each with the rows the
blur reaches above and below; only a band's own rows are read back. The
staging buffers are kept and grown between operations (at most 64 MB held).
Color codecs reproduce egui's nonlinear premultiplication. U16/F32
documents, unsupported devices and unsupported operations retain CPU
processing. Gradients are painted on the CPU: their rows go straight into
the tiles in parallel, which a GPU round trip doesn't beat.

The first use of each filter (and blur reach) starts the driver up, sizes
the buffers, then measures both paths and checks their output. Subsequent
calls use GPU processing only when it was at least 10% faster. GPU command
execution and transfers are included.

Measured on AMD Radeon 780M, Vulkan/RADV Mesa 26.2.2, optimized test profile:

| Operation | Size | CPU | GPU, including transfers |
| --- | --- | ---: | ---: |
| Brightness/contrast | 2048² | 4.61 ms | 33.09 ms |
| Hue/saturation | 2048² | 31.48 ms | 16.72 ms |
| Gaussian blur, radius 4 | 2048² | 66.38 ms | 18.87 ms |
| Gaussian blur, radius 30 | 2048² | 114.34 ms | 37.32 ms |
| Sharpen, radius 4 | 2048² | 118.65 ms | 19.30 ms |
| Brightness/contrast | 4096² | 17.36 ms | 72.84 ms |
| Hue/saturation | 4096² | 98.86 ms | 74.86 ms |
| Gaussian blur, radius 4 | 4096² | 732.15 ms | 114.19 ms |
| Gaussian blur, radius 30 | 4096² | 739.30 ms | 175.12 ms |
| Sharpen, radius 4 | 4096² | 782.66 ms | 118.34 ms |

Reproduce with `cargo test --lib gpu_operation_timings -- --ignored --nocapture`.
The report is also written to `rusty-gpu-timings.txt` in the temporary directory.
These are local measurements, not guarantees for other devices or radii.

## Remaining GPU migration

The current integration reads GPU results back into CPU tiles for previews and
commits. **GPU-resident previews with zero readback are not implemented.**
Adjustment-layer and editable gradient-fill compositing still use their
existing paths. The first priority is a persistent GPU tile cache and preview
composition that can substitute the active layer while preserving masks,
groups, clipping, blend space, and layers above it. That removes the transfer
cost which currently makes simple adjustments slower on GPU.

After that foundation, investigate these candidates in order:

1. **Transform and warp previews:** repeated sampling of unchanged source pixels
   is a strong GPU fit. Preserve the CPU interpolation and selection reference.
2. **Liquify:** deformation-field updates and resampling suit GPU passes; bound
   changes to affected tiles and preserve cancellation/reconstruction.
3. **Layer compositing:** potentially broad benefit, but tree semantics and deep
   documents make this a larger migration. Extend the existing blend-reference
   tests before routing it automatically.
4. **Wet-paint simulation:** local stencil updates are promising; water/pigment
   state and undo need a persistent GPU representation first.
5. **Flood fill and content-aware fill:** retain CPU implementations until
   profiling demonstrates a bottleneck. Connectivity, convergence, and
   synchronization make migration less direct than pixel filters.

These later candidates are ranked by implementation structure and transfer
requirements; no GPU speedup has been measured for them yet.
