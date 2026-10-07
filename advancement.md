What's left, against Krita (K), Clip Studio Paint (C), Procreate (P), ibisPaint (I) and Photoshop (PS)

Brushes and paint behaviour are at Krita's level now (any input to any setting, combined as Krita combines them; Krita's colour smudge with paint thickness; wet paint; impasto; eleven engine types; brushes imported from Krita, Photoshop, GIMP, MyPaint and Clip Studio). The big gaps are outside the brush engine: animation, comic tools, colour depth and management, and platforms.

Part 1: Brush engine

Engines not here yet

- Shape / "Experiment" engine (K): fills the shape the stroke encloses as you draw it. Krita presets of it import as their tip only.
- A real MyPaint engine: .myb brushes are mapped onto this app's settings, approximately.
- Round marker (K) imports as a plain brush.

Krita engines that came across approximately (from reading Krita's source)

- Colour smudge, legacy algorithm: Krita presets default to it (SmudgeRateUseNewEngine off); here every preset uses the new one's formulas, so older smudge presets mix differently. Also missing: the overlay mode (smudging all layers).
- Spray: per-particle colour sampled from the layer, particle shapes other than the tip, particles turning with the cursor or the drawing angle.
- Grid: line shapes.
- Curve: Krita's cubic mode; its connection line is the pen's own segment, here from the curve's start to the pen.
- Particle: Krita's scale x/y options.
- Tangent normal: direction, rotation and mix modes, elevation sensitivity, canvas rotation and mirroring.
- Bristle: connected paths between dabs, ink depletion curves and weights, soaking ink from the layer; the Krita preset keys for the bristle options are read as documented but weren't checked against a real preset.
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

- Full depth for the tools still working at 8 bits in 16-bit and float documents: transforms, smudging and mixing brushes, wet paint, blurs and other spatial filters, fills, liquify.
- The app's "linear light" decodes every profile with sRGB's curve; live shader layers show without the display conversion; CMYK PSD export.
- A mixing palette (C, K), gamut masks (K).

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
