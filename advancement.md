Part 1: Brush engine gaps

The brush roadmap is complete against the basics. What's left is depth and a few whole engines.

Dynamics: the biggest structural gap

- Any input driving any setting (K, M, C, PS). Today you have fixed pairs: pressure → size, opacity and flow, and tilt → size, opacity and angle. Krita and MyPaint let any sensor drive any parameter, each with its own curve:
  - The sensors: pressure, speed, distance, time/fade, drawing angle, tilt direction, tilt elevation, rotation, random per dab, random per stroke, perspective.
  - The parameters: size, opacity, flow, hardness, spacing, scatter, rotation, ratio, texture strength, colour, mix and so on.
  - Of everything below, this unlocks the most. It is a generalisation of your dynamics.rs, not a new engine.
- Missing inputs:
  - Random per stroke (only per-dab randomness exists).
  - Fade over distance or time.
  - Tangential pressure: the airbrush wheel (K, PS).
  - Barrel rotation.
- Pressure driving more settings: hardness, scatter, spacing, texture depth, and dual-brush s
- Colour dynamics:
  - Mix foreground and background colours by pressure (C sub colour, K "Mix", PS).
  - Colour taken from a gradient along the stroke (K).
  - Colour jitter once per stroke, not per dab (P, PS).

Paint behaviour

- Real paint simulation (R, Corel Painter, ArtRage). A wet layer with fluid flow: watercolour, and drips with the tablet's tilt. Paint dries over time, and you can re-wet it. Rebelle andFresco sell on this. Your watercolour edges are the static version.
- Impasto / paint thickness (K "paint thickness", ArtRage, R). A height map painted along wit time.
- Smudge variants:
  - Krita's smearing vs dulling modes.
  - Smudging from all layers rather than just this one.
  - Clip Studio's "colour extension" and "blend with background colour".
  - Photoshop's mixer brush options: load/clean after each stroke, sample all layers.
- Filter brush (K, I): paint any filter through a brush (mosaic, glow, greyscale, posterise).r-pen category. Yours is limited to blur, sharpen and HSB adjust, and you have no filters toreuse yet (see Part 2).

Engines you don't have (K has about 20)

- Spray engine: particle distributions (gaussian, uniform, clustered), particle shapes and rotation, fill density. Yours is up to 16 dabs per step with position jitter.
- Tangent normal: paints normal maps from tilt. Useful for game art.
- Shape / "Experiment" engine: fills the shape your stroke encloses as you draw it.
- Particle, Curve, Grid and Chalk engines: more niche, lower priority.
- Vector brushes (C): strokes stay editable after drawing. You can change their width afterwards, move control points, simplify them, and use a vector eraser that erases up to the next intersection. This is Clip
  Studio's killer feature for line art, and a large project.

Tips and textures

- Define a brush tip from the selection (PS, K clipboard brush, C). You can import tips but cs. This is cheap to add.
- Tip as a colour source: Krita's "lightness / gradient mode" image tips.
- Text as a tip (K).
- Grain that moves with the stroke vs fixed to the canvas (Procreate's "moving vs texturized".
- Texture controls:
  - Grain rotation.
  - Brightness and contrast.
  - A random offset each stroke.
  - Texture applied to each dab, not the whole stroke (PS "texture each tip").
  - Pressure driving texture strength.
- Combining two whole presets (P "combine brushes"). Your dual brush only combines a second tip.

Stroke help

- QuickShape (P): hold at the end of a stroke and it snaps to a line, ellipse, arc or polygon that you can then edit. Very popular.
- Post-correction (C) / curve fitting: smooths the stroke after the pen lifts.
- Pulled-string / lazy-mouse mode (PS, SAI, K): the line trails the pen with a dead zone.
- Motion filtering (P): removes jitter without adding lag.
- Speed-based stabiliser (C): more smoothing when you draw slowly.
- Global pressure calibration from a test stroke (C, P, K). You have curves per setting, but

Brush management and UX

- Tags, favourites, search and a recent-brushes list (K, C, P).
- Pop-up palette: a radial brush and colour ring on a right-click or pen button (K).
- Sub-tool memory (C): each tool remembers its own brush, size and colour.
- Changed presets (K "dirty presets"): a changed preset is marked and can be reset or saved in one click.
- Brush Studio with a live drawing pad (P): test a brush while you edit it.

Part 2: Everything else (the larger gap)

Layers

- Clipping masks: clip a layer to the one below. Layer has alpha_locked but no clipping. ThisStudio, ibisPaint and Procreate for shading, and it's probably the most-missed feature foranyone coming from those apps.
- Adjustment / filter layers and filter masks (K, C, PS). These are non-destructive.
- Reference layer (C): mark lines as the reference for fill and select. You have "layer below" and "all visible" but no marked reference layer.
- Draft layer (C): a layer ignored by fill and export.
- Layer styles:
  - An outline or border effect: Clip Studio's "border effect", used a lot for stickers and t
  - Drop shadow.
- Fill layers and pattern layers.
- Pass-through blending for folders: I didn't check whether groups already support it.
- Layer housekeeping: merge visible, flatten, a new layer from the selection, select from a l thumbnail), and a lock-position option. Some of these may exist in the layers panel; theyaren't in the menus.

Filters and adjustments: there is no Filter menu at all

ibisPaint has about 80 filters. The expected basics:
- Gaussian, motion and radial blur.
- Unsharp mask.
- Levels, curves, HSL and colour balance.
- Gradient map, posterise, threshold and invert.
- Noise, pixelate/mosaic, glow/bloom, chromatic aberration and halftone.
- Line extraction / colour-to-alpha (C, I, K): turns a scanned or photographed sketch into clean lines on transparency. Essential for traditional artists.

You already have map_layer_pixels in src/canvas/storage/pixels.rs, so most of these are small pure functions. Of all the gaps, this one gets you the most per hour of work.

Canvas and image

- No Image menu: I found no resize image, resize canvas, crop, or rotate/flip the image itself (only the view flips).
- Grids:
  - A pixel grid.
  - Square, isometric and perspective grids.
  - Guides you drag out of rulers, with snapping.
- More than 8 bits per channel: 16-bit or 32-bit float colour (K; C partly). You store Color3oft airbrushing will band.
- Colour management (K, PS, C): ICC profiles, soft-proofing and CMYK export for print.

Text and comics

- A text tool: none today. Vertical text and speech balloons (C, I).
- Comic tools (C is the leader, I has most):
  - A panel-border tool that divides frames.
  - Speed-line and focus-line rulers.
  - Screentone and halftone layers.
  - Bleed and trim guides.
  - Multi-page projects.

Animation (K, C, P, I)

- A timeline and frame-by-frame animation, onion skinning, and export to GIF, MP4 and WebM. Procreate's "Animation Assist" shows how small a useful version can be.

Reference and workflow

- Time-lapse recording (P, I, C, K): replays the painting as a video. Artists share these widely, so it's a real growth feature.
- Reference image window (K, C, P): an always-on-top picture you can pick colours from.
- A second view of the same canvas (C sub-view, K new view): a zoomed-out navigator while you paint zoomed in.
- 3D pose figures and head models for reference (C), and 3D model painting (P).
- Colour tools:
  - Colour history.
  - Harmony wheels.
  - A mixing palette (C, K).
  - Gamut masks (K).
  - Intermediate/approximate colour (C).
  - An eyedropper that averages over a radius.
- Selection: grow, shrink, feather and border; quick mask; saved selections; AI "select subjecombine modes and the selection types, so check whether grow/shrink/feather exist.
- Transform:
  - Cage transform and puppet warp (K, PS).
  - Transforming several layers at once.
  - A choice of interpolation (nearest neighbour matters for pixel art).
- Customisation and automation:
  - Rebinding shortcuts (you detect keyboard layouts, but I saw no rebinding).
  - Saved workspaces.
  - Actions / macros (C auto actions, PS).
  - Scripting and plugins (K Python).
- Safety: I found no autosave or crash recovery. Every competitor has it, and losing work is p.
- Several documents open at once, in tabs.

File formats

- PSD import and export. This is the lingua franca between these apps; without it people can't move work in or out.
- Opening .kra, .clip and other apps' .ora files (you write ORA but only open .rpainter).
- Exporting WebP, GIF, PDF and layers as separate files.
- Opening a PNG as a document rather than importing it as a layer.

Platforms and tablets

- No macOS build (the release workflow has Linux, Windows and Android).
- No iPad, where Procreate and ibisPaint live.
- No X11 pressure, which works through XInput2.
- No pen-button or express-key mapping, and no barrel rotation.

AI (optional, and divisive among artists)

- Clip Studio's colorize (AI flat colours) and Krita's fast sketch cleanup (an on-device ML plugin).
