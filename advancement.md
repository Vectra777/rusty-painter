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

Animation (K, C, P, I)

- Frame-by-frame animation and rig layers (Spine, DragonBones and Lottie imports) are in. Still missing for rigs: drawing bones on the canvas and dragging them (they're edited by numbers), building a rig from layers, weight painting, Spine's transform and path constraints and other skins, exporting rigs back to Spine or Lottie, and Moho's and Alight Motion's own project files (no published format: their exported videos and GIFs come in as frames). Keyed motion is in too: any layer's (or animated layer's) position, scale, turn and opacity keyed on the canvas with the Animate tool or by numbers, eased per key (presets or a curve), layers following others, motion paths, and the timeline showing every layer with its keys. The look is keyed too (blur, brightness, contrast, saturation, hue, tint) as is the pivot; every tool (fill, gradient, smudge, shapes, liquify, filters, selections) works on a moved layer where it shows; frames can be picked across layers and copied, cut, pasted, deleted or dragged together; frames are rendered ahead in the background and played from memory. Still missing for frames: audio, keyed filters beyond those six, and opening Krita's animated documents with their frames.

Animation to-do, from what Toon Boom Harmony (TB), TVPaint (TV), Moho (M), After Effects (AE), Alight Motion (AM), Procreate Dreams (PD), Callipeg (CP), Clip Studio Paint (C) and Rive (R) do, most needed first:

- [ ] Audio: sound tracks on the timeline with their waveform, scrubbing (sound as the playhead is dragged), audio in MP4/WebM exports (TB, TV, C, CP, PD).
- [ ] Lip sync: mouth drawings switched from a phoneme track, read from the sound or typed (M, TB, CP).
- [ ] A camera: keyed pan, zoom and turn of the whole shot, with Japanese-style camera moves, and a multiplane camera (layers at depths) (TV, TB, C).
- [ ] Bones built on the canvas: drawing bones joint to joint, binding layers or drawings to them, posing by dragging, IK by dragging the end (M, TB, R).
- [ ] Meshes and weights: one drawing bending smoothly around bones, with painted weights (R, M, Spine).
- [ ] Smart bones / corrective poses: a bone's turn driving drawings or shapes (M).
- [ ] Puppet pins: bending part of a raster drawing by pins placed on it (AE).
- [ ] A full graph editor: each property's value and speed as curves across the timeline, keys with tangent handles, several keys eased at once (AE, AM).
- [ ] Procedural motion: wiggle, loops (cycle, ping-pong), linked properties (expressions), and constraints (follow a path, keep a distance, aim) (AE, R).
- [ ] Physics and dynamics: springy follow-through on keyed or followed layers, particles, gravity and wind (M, R).
- [ ] More keyed effects and masks: glow, shadow, outline, gradient map, displacement, keyed filters from the Filter menu; layer masks and track mattes keyed over time (AM, AE).
- [ ] Motion blur from keyed motion (AE, AM).
- [ ] Exposure sheet: an X-sheet view of the drawings frame by frame, and a timesheet for planning a scene (TB, TV).
- [ ] Performance recording: keys recorded live from dragging a layer as the animation plays (PD).
- [ ] Flipbook drawing: flipping forward to a fresh frame and back with a gesture, onion skins following (PD, CP).
- [ ] Clips and scenes: several shots in one document, with onion skins across a cut (TV).
- [ ] In-betweening of vector drawings: lines morphing between two key drawings (M, TB).
- [ ] Interactive state machines: animations switched by inputs, for apps and games (R).
- [ ] Rig authoring round trips: exporting rigs back to Spine or Lottie.

Known limits of what's in:

- [ ] Liquify and filter-dialog live previews on a moved layer show when the drag ends, not during it.
- [ ] Brush size and dab angle on a turned or scaled layer aren't turned or scaled with it.
- [ ] Effects on a folder are applied to each of its layers in turn (a blur doesn't blend across them).
- [ ] The tint's colour isn't keyed (only how far towards it).
- [ ] Cached playback frames are at most 1600 pixels across and 1 GB in all.

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
