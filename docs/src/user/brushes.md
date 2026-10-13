# Brushes

`B` picks the brush. The brush, eraser (`E`), smudge and blur (`S`) each keep their own brush, size and preset. The brush settings panel opens from the bottom button of the tool strip.

## Brush types

Soft, hard, pixel, bristle, sketch, hatching and image-tip brushes. Image tips are mipmapped and can be any picture; a brush can carry several tips, taken in turn, at random, or by pressure or direction. Edit → Define Brush Tip from Selection makes a tip from the selection.

Engines after Krita's: spray (particle clouds), chalk, curve, grid, tangent normal (normal maps from the pen's tilt) and particle (a swarm the pen pulls along). Krita presets of these import as them.

Ready-made brushes include pencil, ink, calligraphy, charcoal, chalk, bristles, spray, foliage and glow.

## Stroke

Size, hardness, flow, spacing, jitter, scatter, and the airbrush (keeps painting while the pen rests). Every brush has all 28 blend modes (Photoshop's and Krita's Parallel), pixel-perfect mode and anti-aliasing.

## Dynamics

Tapers in and out (no lag), stroke speed, tip angle / squash / follow the stroke, the pen's tilt or its barrel rotation, tips flipped at random, size, opacity, spray and colour randomness, and hard edges from any tip. Each setting has its own pressure curve (pressure can also set the spacing) on top of the pen's own curve, which can be [calibrated](tablets.md#calibration).

## Pen pressure

Brush → Pen pressure & tilt switches pressure onto size, opacity, flow and spacing, with a minimum size and a curve each. These are the Pressure rows of **Inputs** (below), shown here for quick access: there's one place pressure is set, so it can't apply twice. Brushes saved before work as they did.

How pressure is used, as Krita uses it:

- **Spacing follows each dab's size**, whatever sets it (pressure, inputs, tapers, randomness): light strokes don't break into beads.
- **No steps**: pressure isn't rounded, so a big brush or a steep curve widens smoothly.
- **Smoothing carries pressure** with the position: the stabiliser holds the brush back from the pen, and the pressure with it, so a change shows where the brush is.
- Dabs thinner than a pixel fade out rather than leaving a line of dots, and pressing harder with the pen held still grows the mark.
- Each pen sample keeps the time it was taken (Windows Ink, X11, Wayland, Android, iPad), for smoothing, speed and time inputs.

## Inputs

Brush → Inputs: any input (pressure, speed, tilt, direction, distance, time, randomness per dab or per stroke, barrel rotation, an airbrush's wheel) can drive size, opacity, angle, squash, hue, saturation or value through its own curve. Inputs can also drive texture strength, hardness, scatter and the mix with the secondary colour.

## Texture, dual brush and edges

- **Paper textures**: multiply, subtract, height, colour dodge, hard mix. The grain can be turned, move with the stroke, shift each stroke or apply to each dab.
- **Dual brush**: a second tip masking the first.
- **Watercolour edges**, **decoration brushes** in the picture's own colours, and **ribbons** laid along the stroke.

## Colour mixing

Any brush can pick up the paint under it and mix in its own colour, like Krita's Colour Smudge, with all its settings (tip, angle, texture, inputs, scatter, stabiliser, airbrush).

## Wet paint and impasto

- **Wet paint** (Brush → Wet paint): washes that spread, darken at their edges, drip and dry, and water that lifts dry paint.
- **Impasto** (Brush → Impasto, Layer → Impasto): paint laid thick, lit by the layer's light.

## Stabiliser

Simple, dynamic, pulled string (lazy mouse), post-correction (when the pen lifts, or while drawing) or motion filter.

## Smudge and blur

The smudge tool also deforms or clones (Ctrl+click sets the clone source). The blur tool also sharpens or adjusts colour, and its Filter mode paints any [filter](filters.md) through the brush.

## Presets

Selecting a preset keeps your current colour. Right-click a preset to export, delete or tag it; star it as a favourite; search by name or tag, or filter by tag, favourites or recent. The ☰ menu imports and exports sets. Hold `K`, or right-click the canvas, for a pop-up palette of brushes around the pointer.

Your presets save and share as `.rpbrush` files (with their tags and favourite star). Brushes import from:

| App | Files |
|---|---|
| GIMP | `.gbr`, `.gih` |
| Photoshop | `.abr` |
| Krita | `.kpp`, `.bundle` |
| MyPaint | `.myb` |
| Clip Studio | `.sut` |

See [how the brush engine compares](brush-comparison.md) with Krita, Clip Studio and ibisPaint.
