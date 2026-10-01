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
| Image tips (custom shapes) | ✅ PNG, GBR, ABR import | ✅ | ✅ brush patterns | ✅ PNGs in `brushes/`, imported, or made from the selection |
| Image tips sampled cleanly when small (mipmaps) | ✅ | ✅ | ✅ | ✅ trilinear |
| Image tips keep their proportions | ✅ | ✅ | ✅ | ✅ |
| Several tips per brush (random or in sequence) | ✅ animated GIH | ✅ | ⚠️ | ✅ in turn, random, pressure, direction |
| Tip squash (ratio) and fixed angle | ✅ | ✅ | ✅ | ✅ |
| Tip turns with the stroke direction | ✅ | ✅ | ✅ start/end angle | ✅ |
| Random tip rotation | ✅ | ✅ | ✅ | ✅ |
| Tip flip (mirror) | ✅ | ✅ | ⚠️ | ✅ left-right, top-bottom; at random or by any input |
| Hard edges from a soft tip (sharpness threshold) | ✅ | ⚠️ | ❌ | ✅ |
| Aliased pixel brush | ✅ | ✅ | ✅ | ✅ |
| Pixel-perfect lines | ❌ | ❌ | ❌ | ✅ |

## What drives the brush (dynamics)

| Feature | Krita | Clip Studio | ibisPaint | Rusty Painter |
|---|---|---|---|---|
| Pressure → size / opacity / flow | ✅ | ✅ | ✅ | ✅ |
| Own pressure curve per setting | ✅ | ✅ | ⚠️ | ✅ size, opacity, flow, spacing |
| Any input → any setting, with a curve | ✅ sensors | ⚠️ fixed pairs | ❌ | ✅ 14 inputs (incl. fade in dabs and perspective from the perspective assistants) → size, opacity, angle, squash, hue, saturation, value, texture strength, hardness, softness, mirror, scatter, secondary colour mix |
| Pen tilt / barrel rotation | ✅ | ✅ | ⚠️ barrel roll (Apple Pencil Pro) | ✅ tilt → size, opacity, tip angle; barrel rotation and an airbrush's wheel turn the tip or drive any setting (where the tablet reports them: Wayland, X11) |
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
| Brush blend modes (multiply, add, glow…) | ✅ every layer mode | ✅ | ✅ | ✅ all 28 layer modes (with Krita's Parallel) |
| Wet paint / colour mixing while painting | ✅ Color Smudge engine | ✅ colour mixing | ✅ Water type | ✅ any brush (Colour mixing: smudge length, colour rate, by pressure), or the Smudge tool with a colour rate |
| Paper / grain texture | ✅ | ✅ | ✅ | ✅ 5 built-in + your own; multiply, subtract, height, colour dodge, hard mix; turned, moving with the stroke, offset each stroke, or on each dab |
| Dual brush (a second tip as a mask) | ✅ masked brush | ✅ | ❌ | ✅ multiply, darken, subtract, height |
| Watercolour edges | ⚠️ through presets | ✅ | ⚠️ Wet Edge filter | ✅ when the pen lifts |
| Airbrush (keeps painting while held still) | ✅ | ✅ | ⚠️ | ✅ dabs per second |
| Bristle / hair engine | ✅ | ⚠️ through tips | ❌ | ✅ hairs fan out and run dry |
| Decoration / ribbon brushes | ✅ | ✅ | ✅ | ✅ colour tips, ribbons |
| Other engines (sketch, hatching, deform, filter, clone…) | ✅ ~20 engines | ⚠️ | ❌ | ✅ sketch, hatching, deform, sharpen/adjust, any filter, clone |
| Smudge and blur tools | ✅ | ✅ | ✅ | ✅ |

## Stroke help and presets

| Feature | Krita | Clip Studio | ibisPaint | Rusty Painter |
|---|---|---|---|---|
| Stabiliser | ✅ several kinds | ✅ + post-correction | ✅ + forced fade | ✅ simple, dynamic (mass/drag), pulled string, post-correction, motion filter |
| Hold to snap to a shape (QuickShape) | ❌ | ⚠️ | ⚠️ | ✅ line, ellipse (turned too), rectangle, polygon |
| Rulers / assistants (perspective, ellipse…) | ✅ | ✅ | ✅ | ✅ ruler, vanishing point, perspective, ellipse, concentric |
| Symmetry / radial mirror | ✅ | ✅ | ✅ | ✅ incl. kaleidoscope |
| Wrap-around (seamless tiles) | ✅ | ⚠️ | ❌ | ✅ brushes, smudge and blur |
| Pen eraser end switches to eraser | ✅ | ✅ | ✅ | ✅ |
| Preset library / sharing | ✅ bundles | ✅ Assets store | ✅ 3000+ online | ✅ built-in presets + your own, kept and shared as `.rpbrush` files; imports GIMP, Photoshop, Krita, MyPaint and Clip Studio brushes; tags, favourites, search, recent, pop-up palette |

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
- [x] **Preset files.** Every preset is a file in `brushes/presets/`: the
  ones that come with the app (`assets/default-brushes.rpbrush`) are
  copied there on first start. A change to the brush is saved into the
  preset it came from, as in Clip Studio; *Reset to default* and *Restore
  default brushes* undo that. Presets can be exported and imported as `.rpbrush` files (one preset or a
  whole set, with the tips and textures they use), from the presets
  window's menu or by dropping a file on the window.
- [x] **Airbrush.** Paint keeps building while the pen is held still, at
  a rate in dabs per second (Stroke → Airbrush; the Soft Airbrush preset).
- [x] **Several tips per brush**, in turn, at random, or by pressure or
  direction. A folder of pictures in `brushes/` is a tip set; right-click
  tips to add them to a brush. The Mixed Leaves preset uses three.
- [x] **Dual brush.** A second tip, with its own size, spacing, scatter
  and count, masks the first (multiply, darken, subtract or height). The
  Dry Media preset uses a spatter tip.
- [x] **Watercolour edges.** When the pen lifts, the middle of the stroke
  thins and its paint gathers at the rim (strength and width). The
  Watercolour preset uses them with a wash on rough paper.
- [x] **Bristle engine.** A row of hairs across the stroke, each painting
  its own line; they fan out with pressure and run dry at their own pace.
  Presets: Oil Bristle, Dry Brush. (For colour picked up from the canvas,
  use the Smudge tool.)
- [x] **Ribbon and decoration brushes.** Colour pictures keep their
  colours and a brush can paint them (flowers, stitches); a ribbon lays the
  picture along the stroke, repeated, its height across it (lace, printed
  ribbons). Presets: Stitches, Chain, Lace Ribbon, Striped Ribbon, Flowers.
- [x] **Sketch and hatching engines.** Sketch joins each point to earlier
  points of the stroke nearby with fine lines; hatching paints parallel
  lines pinned to the canvas, cross-hatching as you press harder. Presets:
  Sketchy Pencil, Cross Hatch.
- [x] **Deform, filter and clone brushes.** The Smudge tool can also
  deform (push, grow, shrink, swirl) or clone (Ctrl+click the source;
  aligned or not, this layer or all); the Blur tool can also sharpen or
  adjust hue, saturation and brightness under the brush.
- [x] **Assistants:** vanishing point, two-point perspective (drag a
  rectangle's corners), ellipse and concentric ellipses; strokes snap to
  them, following curves round rather than across. Saved with the project,
  like the ruler and mirror painting now are.
- [x] **Wrap-around painting** for seamless tiles (View → Wrap Around):
  brush strokes and the Smudge and Blur tools (every mode) carry on across
  the edges, and the view shows the canvas repeated. Fill, liquify,
  gradients and paper textures don't wrap, and the textures' grain only
  meets up at the seam when the canvas is a multiple of its size.
- [x] **Importing other apps' brushes** (presets window → Import brushes…,
  or drop the file on the window): GIMP (`.gbr`, `.gih`), Photoshop
  (`.abr`), Krita (`.kpp` presets and `.bundle` sets), MyPaint (`.myb`) and
  Clip Studio (`.sut`). Tips always come across; the settings that have a
  counterpart here do (size, spacing, hardness, shape, angle, opacity,
  flow, pressure and its curves, scatter; Krita's textures and masking
  brush, colour smudge, rotation, mirror, pressure spacing, sharpness and
  blend mode), and a report lists what didn't. Tested on Krita's own presets
  and bundles. ibisPaint brushes can't be imported: they're only shared as
  QR codes through the app, in an undocumented format.

- [x] **Brush inputs** (Brush → Inputs): any of nine sensors (pressure,
  speed, tilt, tilt direction, stroke direction, distance, time, random per
  dab or per stroke) drives size, opacity, angle, squash, hue, saturation
  or value, each through its own curve. They stack with the fixed dynamics
  above and are saved in `.rpbrush` files.
- [x] **Define a tip from the selection** (Edit → Define Brush Tip from
  Selection): what's drawn there, dark paints and light doesn't; saved in
  `brushes/` and used at once.
- [x] **QuickShape** (View → QuickShape): hold the pen still at the end of a
  stroke and it becomes a clean line, ellipse, rectangle or polygon,
  editable with the Shape tool's handles until applied.

- [x] **More brush inputs:** texture strength, hardness, scatter and the
  mix of the brush colour and the secondary colour (the one X swaps with),
  each through its own curve. The Two-Tone Chalk preset uses them.
- [x] **Filter brush** (Blur tool → Filter): any filter from the Filter
  menu painted through the brush, with its settings (Settings…). Blurs,
  sharpen and pixelate read the layer as it was before the stroke; going
  over a spot again in the same stroke doesn't filter it twice.
- [x] **Texture options** (Brush → Texture): the grain turned to an angle,
  moving with the stroke instead of pinned to the canvas, shifted at random
  each stroke, or applied afresh to each dab.
- [x] **Stabiliser modes** (Brush → Stabilizer → Method): pulled string
  (lazy mouse; the string is drawn while painting, and the line catches up
  to the pen when it lifts), post-correction (the path is smoothed and
  repainted when the pen lifts, still one undo step) and motion filtering
  (smooths slow, shaky lines, leaves fast ones alone).
- [x] **Brush library** (the presets window, `P`): tags (the built-in
  presets come tagged; imports are tagged with their app), favourites (the
  star on each tile), search by name and tag, the last 8 brushes used, and
  a pop-up palette of favourite brushes around the pointer (hold `K`, or
  right-click the canvas).

- [x] **Krita's missing brush options:** colour mixing in any brush
  (Brush → Colour mixing: smudge length and colour rate, each by pressure
  if you like; a mixing brush paints through the Smudge tool's engine, with
  its own tip and blend mode), tips flipped at random, pressure → spacing,
  hard edges (a sharpness threshold), the tip following the pen's barrel
  rotation and Barrel rotation / Airbrush wheel inputs, colour dodge and
  hard mix textures, and the Parallel blend mode for layers and brushes.
  Not yet: a mixing brush's tip doesn't turn, flip or take a texture; of
  the brush's settings it uses size, hardness, spacing, opacity, flow and
  pressure.

- [x] **Painting as Krita does, from its source.** Wash mode is Krita's
  alpha darken: each dab moves the stroke toward its own opacity and never
  past it, flow setting how fast, the stroke's average opacity easing
  pressure changes. Imported textures use Krita's own formula for every
  alpha mode, classic or soft texturing (`KisMaskingBrushCompositeOp`).
  Brushes imported from Krita's colour smudge use its engine (smearing or
  dulling, smear alpha, smudge radius, colour rate squared, the brush's
  blend mode on the paint colour); a mixing brush's tip turns and takes the
  texture. Hard edges follow any input and keep a soft band. Inputs can
  swing a setting both ways, and drive hard edges and darken. Tips can
  paint by lightness or as a gradient map. The Krita importer reads each
  option's sensors as Krita uses them (its curve on or off, the common
  curve), and its hue, saturation, value, darken, gradient colour source,
  sharpness sensors and lightness or gradient map tips.

### Next

- [ ] Pressure calibration from a test stroke.
- [ ] Post-correction while drawing, not only when the pen lifts.
- [ ] Tags and favourites carried in exported `.rpbrush` files.
- [x] **Vector layers** (Layer → New Vector Layer): lines kept as points,
  smoothed, erased whole or in part, thickened, thinned or recoloured
  afterwards. Not yet: moving a line's points by hand, or erasing up to
  where lines cross.
- [ ] Bigger: a spray/particle engine, wet paint simulation and impasto.

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
- **After the second round of features** (preset files through wrap-around
  and importing): the same fixed workloads, counted with `perf stat` in a
  single-codegen-unit build, run within ±0.4% of the instructions they did
  before (plain round, turned and squashed, image tip), and +2.7% for a 3 px
  pixel-art stroke, whose tiny batches feel the per-batch bookkeeping most.
- **The new features' own cost**, same 60-sample stroke of a 60 px brush
  (`feature_stroke_60_samples`; plain about 4.6 ms): airbrush resting 50 ms
  per sample 6.7 ms, dual brush 7.3 ms, wet edges 6.5 ms, colour tip
  8.5 ms, sketch 8.9 ms, bristle (30 hairs) 11 ms, three image tips 14 ms,
  hatching 20 ms, ribbon 21 ms.
- **After the third round** (brush inputs and the layer features): the
  fixed workloads in `examples/fixed_workloads.rs`, counted with `perf
  stat` in single-codegen-unit builds, run within ±0.04% of the instructions
  they did before for strokes (plain, airbrush, paper texture). Four input
  mappings on the same stroke cost about 3.1 ms (`input_mappings`).
- **After the fourth round** (new input targets, texture options, filter
  brush, stabiliser modes): the fixed workloads run within +0.2% of the
  instructions they did before (plain, airbrush, paper texture), counted
  the same way. A turned grain that moves with the stroke costs about 13%
  more than the pinned grain on the same stroke.

- **After the Krita options round** (colour mixing, flips, pressure
  spacing, hard edges, barrel rotation, new texture modes, Parallel): the
  fixed workloads, counted the same way, run at or under the instructions
  they did before: plain stroke −0.02%, airbrush −0.02%, paper texture
  −0.55%, placed grain −0.25% (the per-row extras now sit behind one test,
  and the texture mode is chosen once per row rather than per pixel). The
  new features on the 60-sample stroke (`feature_stroke_60_samples`):
  random flip on an image tip 13 ms (an image tip alone is about 14 ms),
  pressure spacing 4.7 ms, hard edges 3.1 ms, colour dodge and hard mix
  textures 6.0 and 6.3 ms (pinned paper 5.5 ms), Parallel blend 9.3 ms.
  The smudge engine now leaves pixels the tip doesn't reach alone (no
  round trip through linear light): a 60-sample, 80 px smudge stroke went
  from 48.8 to 32.4 ms (`app_bench`), a mixing brush to 32.7 ms, with an
  image tip 36.3 ms, with Parallel 52.7 ms.

- **After the Krita engine round** (alpha-darken wash, Krita texturing,
  Krita colour smudge, input targets): the fixed workloads, against the
  commit before it: paper texture +0.03%, placed grain +0.10%, plain and
  airbrush strokes +0.27% (a branch per dab row between wash and build-up
  coverage; splitting the painter in two to remove it cost more, the
  stamping closure no longer inlining).

## Sources

- [ibisPaint: Details of Brush Parameters](https://ibispaint.com/lecture/index.jsp?no=118&lang=en)
- [ibisPaint: Create Original Brush Patterns](https://ibispaint.com/lecture/index.jsp?no=200&lang=en)
- [ibisPaint: new features](https://ibispaint.com/newFeature.jsp?lang=en)
- [Clip Studio Paint: Sub Tool Detail palette](http://www.clip-studio.com/site/gd_en/csp/userguide/csp_userguide/505_tool_plt/505_tool_category_plt_subtool_detail.htm)
- [Clip Studio Paint: Customizing brush tools](https://help.clip-studio.com/en-us/manual_en/240_brushes/Customizing_brush_tools.htm)
- Krita: the brush engines and brush settings chapters of the Krita manual
  (docs.krita.org).
