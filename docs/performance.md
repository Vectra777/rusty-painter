# Speed and quality

How fast the heavier features are on a large canvas, what was made faster,
and what the quality tests check.

## Running the timings

The timings are ignored tests (they need a release build to mean anything):

```sh
# Every feature on a 4000×4000 canvas (two painted layers):
cargo test --release --lib perf:: -- --ignored --nocapture --test-threads 1
# Also time opening your own .kra / .clip / .psd files:
RP_OPEN=big.kra:other.clip cargo test --release --lib perf::open_other -- --ignored --nocapture
# Filters alone on a 4096×4096 buffer, brush presets, fills, selections:
cargo test --release --lib -- --ignored --nocapture --test-threads 1
# Criterion benchmarks (statistics, comparisons between runs):
cargo bench --features bench
```

The numbers below were measured in a 4-core Linux container. Your machine
will differ; what matters is the relative cost and how it grows.

## Filters on a 4000×4000 layer

Filter → apply to the whole layer, including reading the layer, writing it
back and filing the undo step. "One thread" is the same work on a single
core, which is the only setting measured both before and after the changes
below.

| Filter | One thread, before | One thread, after | All cores, after |
|---|---:|---:|---:|
| Invert | 1.4 s | 0.30 s | 0.11 s |
| Levels | 1.4 s | 0.24 s | 0.10 s |
| Curves | 1.2 s | 0.28 s | 0.12 s |
| Exposure | 1.3 s | 0.24 s | 0.11 s |
| Hue / Saturation | 2.3 s | 1.3 s | 0.37 s |
| Colour Balance | 2.5 s | 1.6 s | 0.50 s |
| Gaussian Blur (4 px) | 2.4 s | 2.2 s | 1.0 s |
| Motion Blur (20 px) | 7.1 s | 4.6 s | 1.5 s |
| Zoom Blur | 47.8 s | 6.5 s | 1.8 s |
| Spin Blur | 63.6 s | 5.2 s | 1.5 s |
| Reduce Noise (Median) | 4.4 s | 4.6 s | 1.2 s |
| Emboss | 4.5 s | 0.77 s | 0.27 s |
| Find Edges | 1.8 s | 0.49 s | 0.22 s |
| Oil Paint | 3.5 s | 2.8 s | 1.4 s |
| Glow | 4.2 s | 2.9 s | 1.2 s |

All the others (desaturate, posterize, threshold, gradient map, sepia,
solarize, temperature, vibrance, sharpen, chromatic aberration, halftone,
vignette, noise, pixelate, dither, clouds, line art) take 0.1–0.6 s on
all cores.

While a filter dialog is open, the preview runs on a smaller copy while a
slider is dragged and exactly once it's let go (about 0.4 s to the exact
result at 4096×4096).

## What was made faster

- **Zoom, spin and motion blur** averaged up to 64 samples along a path
  through every pixel. Steps along a line, a turn about a point, or a zoom
  (spaced geometrically) add up. So the same 64 points are now the sums of
  one step from each of three passes of 4 samples. That is 12 samples a
  pixel instead of 64, mostly close by, read in whole numbers. The result
  stays within two levels (of 255) of the exact average on a hard
  checkerboard, and is the same average apart from slightly softer
  resampling.
- **Unmultiplying colours**: egui's `to_srgba_unmultiplied` goes through
  floats and powers for every pixel, even opaque ones. Every per-pixel path
  now uses the app's table-based `unmultiply`, which gives exactly the same
  values. Lookup filters (levels, curves, invert, exposure, …) became about
  five times faster.
- **Emboss** read two interpolated colours and converted each to
  brightness per pixel. It now makes a brightness map once and reads
  between its values.
- **Undo history**: filing a step compressed older steps by handing every
  one of them to the thread pool, although all but one were already
  compressed. With hundreds of steps, each new step got slower than the
  last. Only raw snapshots are now handed over.
- **The general compositor** (any document with a folder, mask,
  clipping, fill layer, border or adjustment layer) looked up the
  gamma tables three times a pixel and called the full blend-mode
  function even for plain "over". It now looks them up once per tile,
  blends "over" inline, and walks rows instead of working out every
  pixel's tile position: 33% fewer instructions, and flattening such a
  document 30–37% faster.
- **Vector lines**: a shape added to a vector layer drew the layer's lines
  twice, once as they are and once as they were for undo. The undo picture
  is now read from the layer before the change. Each line also only visits
  the 32-pixel pieces of each row it touches. 500 shape lines went from
  12.2 s to 5.1 s, and the time per line no longer grows with the history.

## Everything else on a 4000×4000 canvas (all cores)

| Feature | Time |
|---|---:|
| Flatten two paint layers | 89 ms |
| … with an adjustment layer | 0.24–0.64 s |
| … with a colour / gradient fill layer | 0.20 / 0.26 s |
| … with a 4 / 16 / 40 px border | 0.26 / 0.31 / 0.45 s |
| … with a 500-line vector layer | 0.13 s |
| Rasterise a 500-line vector layer | 4 ms |
| Export PNG / 16-bit PNG | 0.25 / 0.52 s |
| Export JPEG / TIFF / lossless WebP | 0.52 / 0.12 / 0.18 s |
| Export layered PSD / SVG | 0.44 / 0.39 s |
| Open a 4000 px PSD (decode + layers) | 0.33 + 0.09 s |
| Open a 4000 px Krita document (4 layers) | 0.39 + 0.03 s |
| Transform: rotate preview (full quality / while dragging) | 0.28 / 0.11 s |
| Transform: commit | 67 ms |
| Liquify: 300 px drag, 150 px brush | 59 ms |
| Selection: invert / outline | 27 / 20 ms |
| Enclose fill, 3000 px lasso | 0.22 s |
| Palette: extract 16 / recolour the layer | 26 / 53 ms |
| Import a 3000×2000 PNG | 85 ms |

Brush presets paint a stroke in 0.2–9 ms (`dynamics_tests::preset_stroke_times`);
bucket fill and colour select at 4096² take 0.15–0.22 s.

## Known costs left

- **Wet paint** dries in steps of 1/30 s between strokes (at most four
  a frame, as many as fit 8 ms by the last step's time, at least one: a
  big wash dries slower rather than holding up the frames more): one step
  over 64 wet tiles takes about 5 ms (`wet_step_64_tiles`). It runs on the
  UI thread, so a wash of a thousand tiles still costs a frame about 80 ms.
  A wet tile holds about 100 KB (water, pigment, the dry paint and what it
  shows) until it dries.
- **Impasto**: a stroke laying heights costs about a fifth more than
  the same brush flat; a lit layer composites through the general path,
  about twice a plain layer's time where its paint slopes (576 tiles:
  19 ms against 10 ms). Heights are 2 bytes a pixel of painted tiles, and
  a stroke's undo step holds the height tiles it touched.
- **Curve and particle brushes** draw many thin lines per sample: a
  60-sample stroke of a 60 px brush takes about 40 ms (curve) and 50 ms
  (particle, 30 of them), under a millisecond a sample on the stroke
  worker; spray (40 particles a dab) about 25 ms. The grid, chalk and
  tangent normal brushes cost about what a plain brush does.

- **Documents with folders, masks, clipping, fill layers, borders or
  adjustment layers** composite through the general per-pixel path, which
  is still 2–3 times slower than the plain one. Flattening the whole
  4000 px picture (export, copy merged) takes 0.2–0.6 s instead of 0.09 s.
  On screen only the visible tiles are composited.
- **Vector layers keep a full copy of their lines for every undo step.**
  With hundreds of lines and steps this adds up, to tens of MB (counted
  in the undo history's 512 MB budget).
- **Median and oil paint** are 1.2–1.4 s at 4000 px (radius 2 and 4) and
  grow with the radius.

## Damaged files (fuzzing)

Every file reader has a fuzz test that feeds it thousands of damaged copies
of a good file: flipped bits, numbers set to extremes, pieces cut out or
repeated. It must refuse them or open them, and never crash, hang or try
to allocate a huge amount of memory. Projects are also fuzzed with valid
JSON holding nonsense values (sizes, indices, ids, missing parts), then
flattened and taken back and forward through their whole undo history.

```sh
cargo test --release --lib fuzz_ -- --ignored --nocapture
RP_FUZZ_ROUNDS=20000 cargo test --release --lib fuzz_ -- --ignored --nocapture
# A failing input is saved under the temp dir; replay a project one with:
RP_REPLAY=/tmp/rp-fuzz-values-123.bin cargo test --lib project::fuzz_tests::replay -- --ignored
```

Run in a debug build too: integer overflow panics there instead of
wrapping round.

## Quality tests

These run with `cargo test` and check behaviour rather than one fixed
output:

- **Every filter** (`canvas::filter_quality`), at its default and at a
  strong setting:
  - gives the same result every time;
  - filtering a region (as a selection is: its bounds plus the filter's
    reach) matches filtering the whole picture, to within 2 levels;
  - colour filters keep alpha and leave transparent holes transparent;
  - spatial filters don't paint beyond their reach;
  - neutral settings change nothing (`filters::tests`);
  - every filter paints through the filter brush (`blend::mode_tests`).
- **Zoom and spin blur** stay within two levels of the near-exact
  1024-point average.
- **Exports**:
  - PNG, TIFF and WebP give back exactly the picture;
  - 16-bit PNG holds the same values ×257;
  - JPEG stays above 32 dB PSNR on white;
  - the SVG is well-formed XML, and its embedded pictures hold the layer's
    pixels exactly.
- **Imports**:
  - a Krita document made by Krita opens layer by layer, and our rendering
    matches Krita's own picture to within 3 levels (8 and 16-bit);
  - Clip Studio documents (built to the published layout) open with
    folders, masks, paper, offsets and blend modes;
  - damaged or foreign files of every format are refused, never crash;
  - PSD round-trips (`psd::tests`).
- **Curves** are drawn within a quarter pixel of the true Bézier.
- **Vector lines**: undoing and redoing a shape line over other lines
  gives back exactly the pixels before and after.
