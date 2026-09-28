# Brush engine: comparison and progress

Where Rusty Painter's brushes stand against Krita, Clip Studio Paint and
ibisPaint, and the plan to close the gap. The **Rusty Painter** column is
updated as each feature lands; the roadmap at the end tracks the work.

✅ has it · ⚠️ partly, or unsure · ❌ no

The other apps' columns come from their manuals (see [Sources](#sources));
features vary by version, so treat ⚠️ there as "check before relying on it".

## Brush tip

| Feature | Krita | Clip Studio | ibisPaint | Rusty Painter |
|---|---|---|---|---|
| Round tip, hardness / softness curve | ✅ | ✅ | ✅ | ✅ Gaussian or curve |
| Image tips (custom shapes) | ✅ PNG, GBR, ABR import | ✅ | ✅ brush patterns | ✅ PNGs in `brushes/` |
| Image tips sampled cleanly when small (mipmaps) | ✅ | ✅ | ✅ | ✅ trilinear |
| Image tips keep their proportions | ✅ | ✅ | ✅ | ✅ |
| Several tips per brush (random or in sequence) | ✅ animated GIH | ✅ | ⚠️ | ❌ |
| Tip squash (ratio) and fixed angle | ✅ | ✅ | ✅ | ✅ |
| Tip turns with the stroke direction | ✅ | ✅ | ✅ start/end angle | ✅ |
| Random tip rotation | ✅ | ✅ | ✅ | ✅ |
| Aliased pixel brush | ✅ | ✅ | ✅ | ✅ |
| Pixel-perfect lines | ❌ | ❌ | ❌ | ✅ |

## What drives the brush (dynamics)

| Feature | Krita | Clip Studio | ibisPaint | Rusty Painter |
|---|---|---|---|---|
| Pressure → size / opacity / flow | ✅ | ✅ | ✅ | ✅ |
| Own pressure curve per setting | ✅ | ✅ | ⚠️ | ✅ size, opacity, flow |
| Pen tilt / barrel rotation | ✅ | ✅ | ⚠️ barrel roll (Apple Pencil Pro) | ✅ tilt → size, opacity, tip angle (no barrel roll) |
| Stroke speed | ✅ | ✅ | ✅ Dynamic tab | ✅ size and opacity |
| Taper at the start / end of a stroke | ✅ fade | ✅ | ✅ Fade tab | ✅ size and/or opacity, no lag |
| Random size / opacity | ✅ | ✅ | ✅ Jitter tab | ✅ |
| Scatter (random position) | ✅ | ✅ spray | ✅ | ✅ position jitter only |
| Particles / several dabs per step | ✅ spray engine | ✅ | ✅ particle density | ✅ up to 16 per step |
| Colour randomness (hue / saturation / value) | ✅ | ✅ | ✅ | ✅ per dab |

## How the paint behaves

| Feature | Krita | Clip Studio | ibisPaint | Rusty Painter |
|---|---|---|---|---|
| Build-up vs wash | ✅ | ✅ | ⚠️ "Constant Opacity" | ✅ |
| Brush blend modes (multiply, add, glow…) | ✅ every layer mode | ✅ | ✅ | ✅ all 27 layer modes |
| Wet paint / colour mixing while painting | ✅ Color Smudge engine | ✅ colour mixing | ✅ Water type | ✅ Smudge with a colour rate |
| Paper / grain texture | ✅ | ✅ | ✅ | ✅ 5 built-in + your own; multiply, subtract, height |
| Dual brush (a second tip as a mask) | ✅ masked brush | ✅ | ❌ | ❌ |
| Watercolour edges | ⚠️ through presets | ✅ | ⚠️ Wet Edge filter | ❌ |
| Airbrush (keeps painting while held still) | ✅ | ✅ | ⚠️ | ✅ dabs per second |
| Bristle / hair engine | ✅ | ⚠️ through tips | ❌ | ❌ |
| Decoration / ribbon brushes | ✅ | ✅ | ✅ | ❌ |
| Other engines (sketch, hatching, deform, filter, clone…) | ✅ ~20 engines | ⚠️ | ❌ | ❌ |
| Smudge and blur tools | ✅ | ✅ | ✅ | ✅ |

## Stroke help and presets

| Feature | Krita | Clip Studio | ibisPaint | Rusty Painter |
|---|---|---|---|---|
| Stabiliser | ✅ several kinds | ✅ + post-correction | ✅ + forced fade | ✅ simple + dynamic (mass/drag) |
| Rulers / assistants (perspective, ellipse…) | ✅ | ✅ | ✅ | ⚠️ one straight ruler |
| Symmetry / radial mirror | ✅ | ✅ | ✅ | ✅ incl. kaleidoscope |
| Wrap-around (seamless tiles) | ✅ | ⚠️ | ❌ | ❌ |
| Pen eraser end switches to eraser | ✅ | ✅ | ✅ | ✅ |
| Preset library / sharing | ✅ bundles | ✅ Assets store | ✅ 3000+ online | ✅ 16 built-in + your own, kept and shared as `.rpbrush` files |

## Roadmap

Ordered by how much each changes the feel of painting. A box is ticked, and
the tables above updated, only when the feature passes the quality rules
below.

- [x] **1. Stroke tapers and speed.** Taper in and out (size and/or
  opacity, by length in pixels), with no lag while drawing; stroke speed
  driving size and opacity.
- [x] **2. Tip rotation and shape.** Fixed angle, follow the stroke
  direction, random rotation, squash ratio; for round and image tips.
- [x] **3. Randomness per dab.** Size and opacity randomness; several
  dabs per step (spray); hue, saturation and value randomness.
- [x] **4. Texture.** Paper grain on the brush (built-in grains and your own
  images), with scale, strength and how it combines (multiply, subtract,
  height).
- [x] **5. Brush blend modes.** Multiply, screen, add (glow), overlay,
  darken, lighten, colour dodge / burn, and the rest of the layer modes.
- [x] **6. Wet mixing brush.** Picks up the colour under it and carries it
  (colour rate, smudge length, wetness), like Krita's Color Smudge.
- [x] **7. Per-setting curves, then pen tilt.** A pressure curve for each
  of size, opacity and flow; tilt driving angle and size where the tablet
  reports it.
- [x] **Better tip masks.** Mipmapped, bilinear image tips; proportions kept;
  luminance or alpha as the mask; invert. (An image-tip stroke costs about
  3× a round one: two blended mip levels read per pixel.)
- [x] **Texture brushes.** Presets built on the above: pencil (paper
  grain, tapers, tilt), ink pen, calligraphy, multiply marker, glow, chalk,
  charcoal, dry bristles, spray, foliage, spatter; and four generated tips
  (bristles, rough disc, spatter, leaf). For oil-like mixing, the Smudge tool
  with a Colour rate. Every preset is tested to paint and undo exactly.
- [x] **Preset files.** Presets you save are kept in `brushes/presets/`
  and can be exported and imported as `.rpbrush` files (one preset or a
  whole set, with the tips and textures they use), from the presets
  window's menu or by dropping a file on the window.
- [x] **Airbrush.** Paint keeps building while the pen is held still, at
  a rate in dabs per second (Stroke → Airbrush; the Soft Airbrush preset).
- [ ] **Several tips per brush**, in sequence, at random, or by pressure or
  direction.
- [ ] **Dual brush.** A second tip masks the first.
- [ ] **Watercolour edges.** Paint pools at the rim of the stroke.
- [ ] **Bristle engine.**
- [ ] **Ribbon and decoration brushes.**
- [ ] **Sketch and hatching engines.**
- [ ] **Deform, filter and clone brushes.**
- [ ] **Assistants:** vanishing point, perspective, ellipse, concentric.
- [ ] **Wrap-around painting** for seamless tiles.
- [ ] **Importing other apps' brushes:** GIMP (`.gbr`, `.gih`), Photoshop
  (`.abr`), Krita (`.kpp`, `.bundle`), MyPaint (`.myb`), Clip Studio
  (`.sut`). ibisPaint brushes can't be imported: they're only shared as QR
  codes through the app, in an undocumented format.

### Quality rules for every feature

- **Nothing changes when it's off.** Every existing brush paints
  bit-identical pixels (the golden-checksum tests in
  `src/brush_engine/brush.rs`).
- **Behaviour tests.** Each feature has tests that check what it does
  (a taper narrows the ends, a rotated tip is rotated, randomness stays in
  range and is repeatable with a fixed seed, blend modes match their
  formulas, undo restores the stroke exactly).
- **Speed.** `cargo bench --bench brush_bench` compared against the
  baseline saved before this work (`before-brush-engine`): brushes that
  don't use a feature must not get slower; new features get their own
  benchmark cases.
- **The full CI list passes** (fmt, clippy, tests, bench build, docs,
  Android build).

### Measured results

- **Plain brushes, same output:** a 60-sample pressure stroke paints the
  same pixels as before this work (identical checksum and undo tiles). It
  runs about 1.5% more instructions (counted with `perf stat` on a fixed
  workload); wall-clock timings differ by less than this machine's run-to-run
  noise.
- **Cost of each feature**, on a 60-sample stroke of a 60 px brush (plain:
  about 4.4 ms): speed 2.0 ms (it thins the stroke), random size/opacity
  3.8 ms, taper 5.3 ms, turned and squashed tip 5–6 ms, paper texture
  6.9 ms, hue randomness 8.5 ms, multiply blend 9.1 ms, image tip 14 ms.
- **Every built-in brush:** under 5 ms for an 80-sample stroke.

## Sources

- [ibisPaint: Details of Brush Parameters](https://ibispaint.com/lecture/index.jsp?no=118&lang=en)
- [ibisPaint: Create Original Brush Patterns](https://ibispaint.com/lecture/index.jsp?no=200&lang=en)
- [ibisPaint: new features](https://ibispaint.com/newFeature.jsp?lang=en)
- [Clip Studio Paint: Sub Tool Detail palette](http://www.clip-studio.com/site/gd_en/csp/userguide/csp_userguide/505_tool_plt/505_tool_category_plt_subtool_detail.htm)
- [Clip Studio Paint: Customizing brush tools](https://help.clip-studio.com/en-us/manual_en/240_brushes/Customizing_brush_tools.htm)
- Krita: the brush engines and brush settings chapters of the Krita manual
  (docs.krita.org).
