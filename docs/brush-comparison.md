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
| Image tips sampled cleanly when small (mipmaps) | ✅ | ✅ | ✅ | ❌ |
| Image tips keep their proportions | ✅ | ✅ | ✅ | ❌ stretched to a square |
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
| Own pressure curve per setting | ✅ | ✅ | ⚠️ | ⚠️ one global curve + minimum size |
| Pen tilt / barrel rotation | ✅ | ✅ | ⚠️ barrel roll (Apple Pencil Pro) | ❌ |
| Stroke speed | ✅ | ✅ | ✅ Dynamic tab | ✅ size and opacity |
| Taper at the start / end of a stroke | ✅ fade | ✅ | ✅ Fade tab | ✅ size and/or opacity, no lag |
| Random size / opacity | ✅ | ✅ | ✅ Jitter tab | ✅ |
| Scatter (random position) | ✅ | ✅ spray | ✅ | ✅ position jitter only |
| Particles / several dabs per step | ✅ spray engine | ✅ | ✅ particle density | ✅ up to 16 per step |
| Colour randomness (hue / saturation / value) | ✅ | ✅ | ✅ | ❌ |

## How the paint behaves

| Feature | Krita | Clip Studio | ibisPaint | Rusty Painter |
|---|---|---|---|---|
| Build-up vs wash | ✅ | ✅ | ⚠️ "Constant Opacity" | ✅ |
| Brush blend modes (multiply, add, glow…) | ✅ every layer mode | ✅ | ✅ | ❌ normal + erase only |
| Wet paint / colour mixing while painting | ✅ Color Smudge engine | ✅ colour mixing | ✅ Water type | ⚠️ separate smudge tool only |
| Paper / grain texture | ✅ | ✅ | ✅ | ❌ |
| Dual brush (a second tip as a mask) | ✅ masked brush | ✅ | ❌ | ❌ |
| Watercolour edges | ⚠️ through presets | ✅ | ⚠️ Wet Edge filter | ❌ |
| Airbrush (keeps painting while held still) | ✅ | ✅ | ⚠️ | ❌ |
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
| Preset library / sharing | ✅ bundles | ✅ Assets store | ✅ 3000+ online | ⚠️ built-in + save your own |

## Roadmap

Ordered by how much each changes the feel of painting. A box is ticked, and
the tables above updated, only when the feature passes the quality rules
below.

- [x] **1. Stroke tapers and speed.** Taper in and out (size and/or
  opacity, by length in pixels), with no lag while drawing; stroke speed
  driving size and opacity.
- [x] **2. Tip rotation and shape.** Fixed angle, follow the stroke
  direction, random rotation, squash ratio; for round and image tips.
- [ ] **3. Randomness per dab.** Size, opacity and flow randomness; several
  dabs per step (spray) — *done*; hue, saturation and value randomness —
  *to do* (needs the coloured stroke buffer, with 5).
- [ ] **4. Texture.** Paper grain on the brush (built-in grains and your own
  images), with scale, strength and how it combines (multiply, subtract,
  height).
- [ ] **5. Brush blend modes.** Multiply, screen, add (glow), overlay,
  darken, lighten, colour dodge / burn, and the rest of the layer modes.
- [ ] **6. Wet mixing brush.** Picks up the colour under it and carries it
  (colour rate, smudge length, wetness), like Krita's Color Smudge.
- [ ] **7. Per-setting curves, then pen tilt.** A pressure curve for each
  of size, opacity and flow; tilt driving angle and size where the tablet
  reports it.
- [ ] **Better tip masks.** Mipmapped, bilinear image tips; proportions kept;
  luminance or alpha as the mask; invert.
- [ ] **Texture brushes.** Presets built on the above (charcoal, pencil
  grain, chalk, spray, foliage, calligraphy, glow, oil mix).
- [ ] Later: dual brush, airbrush, several tips per brush, decoration
  brushes, more rulers.

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

## Sources

- [ibisPaint: Details of Brush Parameters](https://ibispaint.com/lecture/index.jsp?no=118&lang=en)
- [ibisPaint: Create Original Brush Patterns](https://ibispaint.com/lecture/index.jsp?no=200&lang=en)
- [ibisPaint: new features](https://ibispaint.com/newFeature.jsp?lang=en)
- [Clip Studio Paint: Sub Tool Detail palette](http://www.clip-studio.com/site/gd_en/csp/userguide/csp_userguide/505_tool_plt/505_tool_category_plt_subtool_detail.htm)
- [Clip Studio Paint: Customizing brush tools](https://help.clip-studio.com/en-us/manual_en/240_brushes/Customizing_brush_tools.htm)
- Krita: the brush engines and brush settings chapters of the Krita manual
  (docs.krita.org).
