What's left, against Krita (K), Clip Studio Paint (C), Procreate (P), ibisPaint (I) and Photoshop (PS)

Brushes and paint behaviour are at Krita's level now (any input to any setting, combined as Krita combines them; Krita's colour smudge with paint thickness; wet paint; impasto; eleven engine types; brushes imported from Krita, Photoshop, GIMP, MyPaint and Clip Studio). The big gaps are outside the brush engine: animation, comic tools, colour depth and management, and platforms.

Part 1: Brush engine

Engines not here yet

- Shape / "Experiment" engine (K): fills the shape the stroke encloses as you draw it. Krita presets of it import as their tip only.
- A real MyPaint engine: .myb brushes are mapped onto this app's settings, approximately.
- Round marker (K) imports as a plain brush.

Krita engines that came across approximately (from reading Krita's source)

- Colour smudge, legacy algorithm: Krita presets default to it (SmudgeRateUseNewEngine off); here every preset uses the new one's formulas, so older smudge presets mix differently. Also missing: the overlay mode (smudging all layers).
- Spray: density mode (more particles the bigger the brush), aspect, jitter, per-particle colour (sampled from the layer, mixed with the background by pressure, random HSV or opacity), particle shapes other than the tip, particles turning with the cursor or the drawing angle.
- Grid: cells wider than tall, divisions (by pressure too), random borders, line shapes; Krita repaints the cells under each dab, here each cell is painted once per stroke.
- Curve: Krita's cubic mode; its connection line is the pen's own segment, here from the curve's start to the pen.
- Particle: a different model. Krita starts every particle at one point, each answering the pull at its own rate, drawing dots.
- Tangent normal: direction, rotation and mix modes, elevation sensitivity, canvas rotation and mirroring.
- Bristle: Krita's hairy model (a bristle for each pixel of the tip, cut off by pressure, shear, connected paths, ink depletion curves, saturation depletion, soaking ink from the layer). Krita hairy presets import as the type only.
- Sketch: magnetify, make-connection and distance density don't import.
- Mix colour source with several sensors at once.
- Deform: lens in and out, colour deform. Clone: healing.

Paint behaviour

- A lightness map (paint thickness) stays under later strokes of other brushes; Krita flattens it into the paint as soon as another tool paints. Bake Impasto does it by hand.
- Wet paint: flowing while the pen is still down; drips following the tablet's or phone's tilt; drying runs on the UI thread (a frame budget keeps it smooth, but a wash of a thousand tiles still costs a frame about 80 ms).

Brush management

- Combining two whole presets (P "combine brushes"); the dual brush only adds a second tip.
- Sub-tool memory (C): each tool remembering its own brush, size and colour.
- A live test pad in the brush settings (P Brush Studio).

Part 2: Everything else

Animation (K, C, P, I): the biggest missing area

- A timeline and frame-by-frame animation, onion skinning, export to GIF, MP4 and WebM. Procreate's Animation Assist shows how small a useful version can be. (Time-lapse recording exists and exports a GIF.)

Comics (C leads, I has most)

- Panel borders that divide frames, speed-line and focus-line rulers, speech balloons, screentone layers (a Halftone filter exists), bleed and trim guides, multi-page projects.

Colour

- More than 8 bits per channel: 16-bit or 32-bit float colour (K, PS, partly C). Soft airbrushing can band.
- Colour management: ICC profiles, soft-proofing, CMYK export for print (K, PS, C).
- Colour history, harmony wheels, a mixing palette (C, K), gamut masks (K).

Layers

- Pass-through blending for folders.
- Transforming several layers at once.

Selection and transform

- AI "select subject".
- Cage transform and puppet warp (K, PS).

Workflow

- Several documents open at once, in tabs.
- Saved workspaces.
- Actions / macros (C auto actions, PS) and scripting or plugins (K Python).

Platforms

- No macOS release build (the release workflow builds Linux, Windows and Android).
- No iPad, where Procreate and ibisPaint live.

AI (optional, and divisive among artists)

- Clip Studio's colorize (AI flat colours) and Krita's fast sketch cleanup (an on-device model).

Dependencies

- cargo audit: quick-xml 0.30 (two advisories, about slow parsing of crafted XML) comes in through egui 0.29's accessibility support; it only reads the desktop's accessibility bus. Clearing it means moving to a newer egui.
