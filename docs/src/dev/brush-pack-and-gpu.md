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
