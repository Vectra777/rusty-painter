# Procreate 5.3.15 — reverse-engineering notes (pass 1)

Behaviour notes for building our own versions. No Procreate code or shaders are copied here; everything is described in my own words.
Source: `Procreate_5.3.15_ios_16.0.ipa`, arm64 Mach-O, Swift and ObjC, `cryptid 0` (decrypted), 14.8 MB.
Tools: rizin 0.7.4 (class dump) and Ghidra headless (`-noanalysis`, decompiling chosen functions only).

## Layer map

| Prefix | What it is |
|---|---|
| `Valkyrie*` (457 classes) | The Metal rendering and painting engine: strokes, layers, compositing, filters, modes, 3D |
| `Silica*` | Model and archive classes (`SilicaDocument`, `SilicaLayer`, `SilicaBrush`); these are the `$classname`s in `.procreate` and `.brush` archives |
| `Si*` | In-house UI kit: gesture recognisers, cardover panels, undo manager, zip, PSD writer |
| `ProcreateSwift`, `BrushStudio`, `ColorPanel`, `Layers`, `Gallery`… | Swift app layer and UI |
| `AdobeFileStreams` | PSD and ABR reading and writing |

1,417 classes and 19,446 methods in total. The full dump is in the scratchpad (`classes.txt`).

The engine is driven by **modes**. Each tool is a `Valkyrie*Mode` with a `*ModeRenderer`, run by `ValkyrieModeQueue` through start, tick and shutdown (`ValkyrieModeStartParameters`, `ModeTickParameters`, `ModeTickResult`, `ModeShutdownState`). Modes: Stroke, StrokePreview, StrokeAnimated, Selection, Flood (ColorDrop), Liquify, Transform, Transform3D, Filter, Text, LayerSelect, Merge, Copy, Duplicate, Clear, Resize, Composite, TextureEdit.

## Shaders (`ValkyrieMetal.metallib`, 152 functions: 66 fragment, 43 vertex, 43 kernel)

The full list is in the scratchpad (`metal_funcs.txt`). Grouped:
- **Brush**: `StrokeSynthesize` (stamp instances → stroke buffer), `StrokeSynthesizePatch`, `StrokeTransferComponents` (stroke buffer → layer, split by colour, roughness and metallic), `KernelSmudge` and `SmudgeInitialize{VS,PS}`, `KernelFastpathTransfer`, `Particles*` (particle brushes rasterised per tile: `ParticlesTiles`, `ParticlesQuad`, `ParticlesRasterize`).
- **Composite**: `FragmentBlend` (layer blend modes), `FragmentMask`/`MaskTransfer` (clipping and masks), `KernelPyrLevelBlend`, `LayerSelect`.
- **Tiles and housekeeping**: `KernelTileEmpty` (the GPU checks whether a tile is empty, so empty tiles are not stored), `KernelTextureAABB` (content bounds), `KernelQuadClear`, `IntegerDownscale` and `Downscaler` (mips and thumbnails), `PackageMap`.
- **Filters**: Blur (makes its coefficients on the GPU), ChromaticAberration ×3, Halftone (prefilter and combine), Dilate, Max, `ApplyLUT`, and `FilterSynthesize`/`FilterCommit` (a preview pass, then a commit pass).
- **Liquify**: `LiquifyKaosParticle`/`KaosResolve` (particle displacement field), `LiquifyPuck`, `KernelLiquify`.
- **Patch synthesis**: `TakePatch`, `PatchMatcher`, `MinimumValueFinder`, `ValuePositionFinder` (with `ValkyrieStitcher`: Laplacian pyramid and patch match for seamless texture edges).
- **Palette from an image**: `ImageIndexer_*` (colour usage statistics → clustering → palettise).
- **3D painting**: deferred shading, prepass, skybox, triangle IDs, mesh selection, UV render; PBR with the `ibl_brdf_lut.png` lookup table.

## Stroke pipeline

```
touch → ValkyrieStrokeSegment        (one per input point: pressure, speed, tilt, roll, taper)
      → ValkyrieStrokePlotControl     (point, speed, pressure, azimuth, altitude, abs/rel roll,
                                       size, opacity, bleed, mixture, blur, interval)
      → ValkyrieStrokePlot            (stamps placed along a curve; renderInstancesToBuffer:symmetry:variance:)
      → StrokeSynthesize shader       (stamps → stroke buffer, grain applied)
      → StrokeTransferComponents      (stroke buffer → layer tiles)
      ValkyrieStrokeModeRollback      (keeps the touched tiles and roughness/metallic buffers: undo is a tile snapshot per stroke)
```

What the decompiled functions do (paraphrased; Ghidra without analysis, so some arguments may be mislabelled):

- **Size unit**: `lerpAndClampSize` interpolates between min and max size by pressure × zoom, multiplies by **128**, clamps to [minSize, maxSize] and multiplies by a forced scale. So `maxSize = 1.0` means a 128 px radius scale.
- **Pressure → size** (`sizeForPressure:speed:roundnessPressure:`): `dynamicsPressureSize` p gives factor `1 − p·(1 − pressure)` for positive p and `1 + p·pressure`-like inversion for negative p. The result is multiplied by a speed factor and by a tilt factor (blended by `min(roundnessPressure, 1)`). Each axis then goes through its `…Curve` (`ValkyrieMagnitudinalCurve`).
- **Curves** (`ValkyrieMagnitudinalCurve`): a **natural cubic spline** (it solves for second derivatives, with the usual `/6` terms) through points stored as strings ("{x, y}"), sampled into a table (`getPreparedSplineCurve:count:stride:buffer:`).
- **Speed** (`speedWithTimeDilation`): the distance per time step is mapped through a square root of an affine term with constants **−25** and **100**, then clamped to [0, 1]. It is then smoothed exponentially with weight `3/dt` against the previous speed, unless a mode flag asks for the raw value.
- **Pressure softening** (`softenedPressure`, `dynamicsPressureSmoothing`): it raises a dt ratio to the power `2·speed` to blend towards the previous pressure, then applies an exponential smoothing with a `2/dt` weight that ramps in over the first few samples.
- **Taper**: size lerps towards **0.1** by `taperSize·(1 − t)` with a power from `taperShape`. Opacity is multiplied by `1 − (taperOpacity·(1 − t))^2.5`. Pencil-only tapers (`pencilTaper*`) are used instead when the input is an Apple Pencil.
- **Stamp spacing** (`plotCurve:previousPlot:initialAngle:`): the control points are first smoothed with a **[¼, ½, ¼]** kernel. The step is roughly `0.5 + 1.5·size/128 + plotSpacing·(2·size + 1)`, so there is a floor even at 0 spacing. Positions along the segment are warped by a `x + k·(x|x| − x)` easing (a speed-dependent bunching). Jitter variance is computed per plot (`calculateVariance`).
- **StreamLine** (`ValkyrieStrokeFFTSmoothing`, `plotFFTSmoothingAmount/Bias/Kind`): each axis is split and padded (mirrored at the ends) and filtered in the frequency domain. `plotMovingAverageStabilization` is a separate moving average, and `plotSmoothing` is a third smoother.
- **Smudge** (`KernelSmudge`): a compute kernel with `accumulator`, `carryoverIn` and `carryoverOut` (a ping-pong pickup buffer), and a gamma-correct variant (`blendGammaCorrect`).

## Canvas and tiles

- `ValkyrieTile` is an integer (col, row) with a `positionKey`. `rectBoundedBy:withTileSize:` gives `col·T, row·T`, clipped to the canvas size (edge tiles are smaller). T comes from the document (`tileSize`, 256 in the files I've seen).
- `ValkyrieLayerChunk` holds a tile, its rect, a file path, `isLZ4`, `isEmpty` and `isTemporary`. Each tile is its own LZ4 file on disk, loaded lazily (`ValkyrieLayerLoadBatcher` and slots, `ValkyrieLayerLoadOperation` and `SaveOperation`). Empty tiles are found on the GPU and dropped.
- Memory: `ValkyrieHeap` with ping and pong heaps, `CachedMemorylessTexture`/`MemorylessTextureCache` (tile-memory render targets on Apple GPUs), and `ArgumentsSuballocator`.
- The view (`ValkyrieView`, `ValkyrieCamera`, `ValkyrieRemapScreenToTexture` and its reverse) composites with `ValkyrieCompositeAccumulator`. `ValkyrieComposite*Effect` applies liquify and transform previews while compositing instead of writing pixels until commit.

## Other tools (class evidence only; not decompiled yet)

QuickShape: `ShapeDetection`, `ShapeHeuristics`, `ShapeCanonicalPolyline`, `ShapePossibilityArray`, `ShapeAnimator`. Snapping: `Snapper` with point and segment elements. Drawing guides (perspective and symmetry): `DrawingGuide`, `PerspectiveControlPoint`. Warp: `WarpPatch`, `WarpGeometry(Cache)`. ColorDrop: `FloodMode` with `FloodModeAdjustment` (dragging sets the threshold). Text: `TypesetText`, `TextImposter` (text drawn to a texture). Animation Assist: `DocumentAnimation`, `CompositeModePlayback*`.

## Brush format: 243 keys in the 221 built-in brushes; `import/procreate.rs` reads 43

All keys with their ranges are in the scratchpad (`brush_keys.txt`). The unread keys that matter most for appearance:
- **Curves**: `dynamicsPressure{Size,Opacity,Bleed,Hue,Saturation,Brightness,SecondaryColor,TransferModulation,GrainDepth,…}Curve` (`ValkyrieMagnitudinalCurve`, points stored as strings)
- **Speed and smoothing**: `dynamicsSpeedSize`, `dynamicsSpeedOpacity`, `dynamicsPressureSmoothing`, `dynamicsPressureResponse`, `plotSmoothing`, `plotMovingAverageStabilization`, `plotFFTSmoothing*`
- **Tilt**: `dynamicsTilt{Size,Opacity,Bleed,Gradation,Compression}`, `*TiltAngle`
- **Wet mix**: `dynamicsMix`, `dynamicsLoad`, `dynamicsWetAccumulation`, `dynamicsSmudgeAccumulation`, `dynamicsGlazedFlow`, `dynamicsGlazeType`, `renderingRecursiveMixing`, `renderingMaxTransfer`, `renderingModulatedTransfer`, `wetEdgesAmount`, `burntEdgesAmount`/`BlendMode`
- **Taper**: `taperPressure`, `taperShape`, `taperSizeLinked`, `pencilTaper*`
- **Shape and grain**: `shapeCount`/`CountJitter`, `shapeRoundness`, `shapeFlip{X,Y}Jitter`, `shapeAzimuth`, `grainBlendMode`, `grainDepthMinimum`/`Jitter`, `textureZoom`, `textureOffsetJitter`, `texturizedGrainFollowsCamera`
- **Colour dynamics**: `dynamicsJitter{Hue,Saturation,Lightness,Darkness}` (per stamp) and `dynamicsJitterStroke*` (per stroke)
- **Per-tool sizes**: `paintSize`, `smudgeSize` and `eraseSize` are separate, as are their opacities
- **Materials**: `metallic*`, `roughness*`, `height*`

## Pass 2: shader maths (from the AIR bitcode via `llvm-dis`, paraphrased)

All 152 shaders disassemble cleanly with LLVM 22. They work in **half precision**, on **premultiplied** colour, and switch features with Metal function constants (`blend_mode`, `grain_blend_mode`, `gamma_correct_mode`, `erase_mode`, `masked`…).

### Layer blend (`ValkyrieFragmentBlend`, `inline_blend`): base = layer below, s = layer drawn on top
Most modes unpremultiply both colours, compute the separable result f(b, s) and recombine with one shared mixer: `rgb = (1−aₛ)·b + aₛ·mix(s_rgb, f(b,s), a_b)`, `a = aₛ + (1−aₛ)·a_b`. This is exactly the W3C compositing formula, so standard maths reproduces it. Internal ids:
- 11 normal (source-over), 34 behind (destination-over), 12 multiply, 13 screen, 14 add (saturated), 15 lighten (max, premultiplied), 35 darken (min against the source-over result)
- 21 exclusion, 22 difference, 23 subtract (b − s, saturated), 24 linear burn (b + s − 1, max 0)
- 25 colour dodge `b / max(1 − s, ε)` and 26 colour burn `1 − (1 − b) / max(s, ε)`, with ε = half 0x068E ≈ 1e-4
- 27 overlay, 28 hard light (overlay with the arguments swapped), 33 soft light
- 29–32 hue, saturation, colour and luminosity via the W3C `setLum`, `setSat` and `clip` functions (luma from `rgb2y`)
- 40 lighter colour and 41 darker colour (choose the whole pixel by its YUV Y)
- 36–39 and 42 go per channel through `blendChannel`: vivid light, linear light, pin light and hard mix in `channelblend_*`
- 2–10 are mask and channel utilities (broadcast one channel, lerp by one channel); 45 erase (`b·(1−aₛ)`); 46 binary coverage
- 16–20 are special: a multiply whose strength is scaled by the colour distance |result − base| (used for masks and tinting)

The document's `blendMode` numbers are mapped to these internal ids in Swift (not traced yet).

### Stamp synthesis (`ValkyrieFragmentStrokeSynthesize`)
Parameter block: `secondaryColor`, `grainSourceAliasing`, `grainLOD`, `alpha`, `wetEdgesAmount`, `burntEdgesAmount`, `depth`, `contrast`, `brightness`.
- **Grain** (`blendGrain`): first g' = saturate((contrast + 1)·(g − 0.5) + brightness + 0.5). Then by grain mode:
  - "height" (internal 43): saturate(12·depth·coverage − g')
  - "linear height" (44): max(12·depth·coverage·(1 − g'), saturate(12·depth·coverage − g'))
  - any other layer blend mode (per channel): mix(coverage, blend(coverage, g'), depth)

  `textureInverted` flips g first. Grain can be per stroke (`grain_per_stroke_mode`) or per stamp.
- **Wet edges**: coverage c becomes mix(c, 1 − |sin(4.5c)/(4.5c)|, wetEdgesAmount). It is a sinc-shaped ring, so mid-coverage turns opaque and the centre dips.
- **Burnt edges**: weight = |1 − c|·burntEdgesAmount. The stroke is blended over the original with `burntEdgesBlendMode`, and the result is mixed in by that weight.
- **Gamma-correct mix** (`gammaMix`, used when `blendGammaCorrect`): it squares the unpremultiplied colours (γ = 2), mixes, then takes the square root. This is cheap linear-light blending.
- The secondary brush (dual brush) is combined with `blendSecondaryBrush` (the same per-channel modes, or linear height).
- **Predicted strokes**: the renderer draws Apple Pencil predicted points into separate "predicted" attachments and throws them away on the next event (`predictedSynthesizedStroke*`, `encodePredictedPrimaryParameters…`). This is a big part of why Procreate feels like it has no latency.

### Smudge (`ValkyrieKernelSmudge`, compute)
Each stamp instance has colour, point, size, angle, mixture, pull and grade. For each pixel the kernel walks the stamps covering it, in order:
1. carry = mix(canvas sample, carry, pull)
2. carry = mix(carry, brush colour or override colour, mixture)
3. out = mix(original, carry, grade)

With gamma correction on, samples are squared before mixing and square-rooted after. The carry is saved between dispatches in a carryover texture (`carryoverIn`/`carryoverOut`, `isLoadingCarryover`/`isSavingCarryover`), so the pickup continues across stroke segments.

## Pass 2: engine (Ghidra; the full analysis was still running when this was written)

- **Memory model: no tile eviction while painting.** Layer memory = width × height × channels. Max layers = layer budget ÷ that (`maximumStandardLayerCountFor2DDocumentOnDeviceMemoryInBytes:size2D:`), with the budget derived from device RAM (`layerMemoryForDeviceMemoryInBytes:`). This is why Procreate caps the layer count per canvas size. Every layer of the open document is a resident GPU texture.
- **Tiles are a disk and commit unit, not a memory unit.** `ValkyrieLayerStatus` is a state machine: unloaded → loading (counting tiles) → loaded → committing (counting tiles) → loaded, plus temporary and permanent states. Saves write only `modifiedTiles`. Each chunk is LZ4 and has a revision extension (`revisionExtensionOfTile:inIndex:`), so a save writes new tile files instead of rewriting a layer.
- **Loading**: `ValkyrieLayerLoadOperation` (an NSOperation per chunk) decompresses into a slot of a shared staging buffer (`ValkyrieLayerLoadBatcher` and slots). When the buffer fills, `tryFlush` sends one blit for many tiles.
- **Undo**: `ValkyrieStrokeModeRollback` keeps the original tiles under a stroke's bounds. The renderer tracks `uncommittedTiles` and only transfers those on commit.

### ColorDrop (`ValkyrieFloodModeRenderer`)
- A CPU **scanline flood fill** on 8-bit data (1, 2 or 4 channels). The distance is the **maximum per-channel absolute difference** (Chebyshev, done in NEON) from the colour under the start point.
- A selection or mask byte is folded in as `max(dist, 255 − mask)`. A pixel is in if the distance ≤ threshold (0–255). Each row grows left and right, with a visited byte map, and the rows above and below are queued.
- Dragging after the drop updates `threshold` live (`FloodModeAdjustment`), and the fill reruns.
- The fill itself (`floodColor:…:gradient:preserveAlpha:mix:threshold:deblend:`) runs on the GPU. **Deblend** removes the old colour from anti-aliased edge pixels so there's no halo.

### Liquify (`ValkyrieLiquifyParticles` and `KernelLiquify`)
- Brush motion spawns particles (`spawnWithChaos:momentum:`; "crystals" and wake variants for Crystals and Edge). Particles move a displacement texture, and the image is resampled through it at commit.
- Each step: pos += vel·dt. Velocity is damped by `0.968^((dt/0.008)/(momentum + 0.001))`, with a floor of 1e-5. Life drops by dt/(momentum + 0.001), or by dt for crystal particles (type 9).
- Brush velocity is smoothed over draw points with a factor of 40·dt (`updateSmoothedVelocityForDelta:`).

### Constants found
- 0.2 is the dt-ratio clamp in pressure softening.
- 2.2 is the exponent in a plot falloff term `(1 − x)^2.2`.
- 1/128 is the size unit (see above).

## Pass 3: full Ghidra analysis (ObjC names resolved)

### How a brush archive is loaded (`ValkyrieBrush initWithCoder:`)
- About 160 keys are read. **Almost no clamping on load**: values are taken as stored, and limits apply later (UI sliders, `maximumMaxSize`, and `lerpAndClampSize` at render time). For an importer this means reading the values raw and clamping where they are used.
- Fallbacks when a key is missing (from older brushes):
  - `pencilTaper*` copy the matching `taper*` (finger) values.
  - Every `*TiltAngle` copies `dynamicsTiltAngle`.
  - `dynamicsWetAccumulation` copies `dynamicsSmudgeAccumulation`.
  - `plotSpacingVersion` is 1 if the archive has `dualBlendMode` (a newer brush), otherwise 0. This switches between the old and new spacing curves.
  - `shapeFilter`, `textureFilter`, `textureOffsetJitter`, `pencilTipAnimation`, `hoverFill`, `hoverOutline` and `texturizedGrainFollowsCamera` default to on (1).
  - `grainBlendMode` and `burntEdgesBlendMode` default to 1. `dualBlendMode`, `shapeFilterMode`, `textureFilterMode` and `shapeRollMode` default to 0. `dynamicsGlazedFlow` defaults to 1.0.
- If `textureOrientation` is present and `grainOrientation` is not, the old orientation value is remapped through a three-entry table and `textureInverted` is cleared.
- **Rendering mode**: `dynamicsRenderType` sets the mixing flags. Old brushes derive it from `dynamicsGlazeType`, or failing that from the boolean `dynamicsGlaze` (true = 1, false = 3).

  | renderType | recursiveMixing | maxTransfer | modulatedTransfer | other |
  |---|---|---|---|---|
  | 0 | 1 | 0 | 0 | mix = 0, glazedFlow = 1 |
  | 1 | 0 | 0 | 0 | mix = 0 |
  | 2 | 0 | 1 | 0 | mix = 0 |
  | 3 | 1 | 0 | 0 | glazedFlow = 1 |

### Stroke renderer (`ValkyrieStrokeModeRenderer`)
- Stamps are rasterised in **64 px screen-space tiles** (`screenSpaceTileSize`). Only the touched tiles are synthesised and transferred (`uncommittedTiles`).
- **Smudge buffer size**: 1024 px on high-end GPUs and 512 px otherwise. In one mode it is instead max(viewport)/4, rounded up to a multiple of 16, with a minimum of 128. The carryover strip is size/8 tall, and the working height is size − 2·(size/8).
- Memory comes from two heaps sized up front for the document and viewport (`ping`/`pong…RequiredLengthForDocumentSize:viewportSize:`). Prediction (Apple Pencil) needs its own extra space (`requiredPredictedLength…`).
- Plots are drawn in groups (`renderPlotsGrouped:groupSize:`). The pipeline is primary, secondary (dual brush), then predicted, then secondary predicted, and simple brushes skip the extra pass (`fastpathTransferFromOriginal`).

### QuickShape (`ValkyrieShapeHeuristics`, `ValkyrieShapeDetection`)
- When the hold fires, the stroke's vertices (with timing) are fitted to a line, polyline, triangle, quadrilateral and ellipse. Each fit also gives a specialised error: circle and arc for the ellipse, equilateral for the triangle, rectangle and square for the quadrilateral.
- Errors are normalised by the squared bounding-box diagonal and weighted by min(diagonal·k/30, 1)². Ellipse error is also divided by (w/h + 2); triangle and quadrilateral errors are halved; arc gets ×3. Ellipse and circle swap places when an arc is also detected and the two scores are within a factor of 2.
- The candidates are sorted by score. Every candidate after the first whose score stays ≤ 4 forms the "progression", which is what Edit Shape and the finger-tap-to-regularise offer. Trailing points (`pointCountExcludingTrashAtEndOfVertices`) are trimmed first, so the wobble while holding still doesn't count.

## Status
All three requested passes are done.

## Pass 4: the three open items

### Document blend mode → shader id (`ValkyrieLayer fragmentBlendMode:`, a plain lookup table)
Each document number is matched to the shader formula found in pass 2. The names are inferred from those formulas.

| doc | shader | mode | doc | shader | mode |
|---|---|---|---|---|---|
| 0 | 11 | Normal | 17 | 33 | Soft Light |
| 1 | 12 | Multiply | 18 | 34 | Behind (destination-over) |
| 2 | 13 | Screen | 19 | 35 | Darken |
| 3 | 14 | Add | 20 | 36 | Hard Mix (per channel) |
| 4 | 15 | Lighten | 21 | 37 | Vivid Light (per channel) |
| 5 | 21 | Exclusion | 22 | 38 | Linear Light (per channel) |
| 6 | 22 | Difference | 23 | 39 | Pin Light (per channel) |
| 7 | 23 | Subtract | 24 | 40 | Lighter Color |
| 8 | 24 | Linear Burn | 25 | 41 | Darker Color |
| 9 | 25 | Color Dodge | 26 | 42 | Divide (per channel) |
| 10 | 26 | Color Burn | 27 | 43 | Height (grain only) |
| 11 | 27 | Overlay | 28 | 44 | Linear Height (grain only) |
| 12 | 28 | Hard Light | 29–33 | 16–20 | distance-weighted multiply variants |
| 13–16 | 29–32 | Color, Luminosity, Hue, Saturation (non-separable) | 34 | — | special case (separate function) |

Anything else falls back to 11 (Normal). The same table is used for brush `blendMode`, `grainBlendMode`, `dualBlendMode` and `burntEdgesBlendMode`.

### QuickShape fits (C++ helpers under `ValkyrieShapeDetection`)
- **Ellipse**: this is a **direct least-squares conic fit** (the published Fitzgibbon method).
  - Points are first normalised to the bounding box (centre, half-extents). Strokes smaller than a minimum size are rejected.
  - Each point gives a row [x², xy, y², x, y, 1]. The 6×6 scatter matrix DᵀD is built with vDSP, and a constrained eigen solve gives the conic (a, b, c, d, e, f).
  - The conic is converted to centre, semi-axes and angle with the standard closed-form formulas (discriminant b² − 4ac; the angle is axis-aligned when b = 0). The fit fails on NaN or infinite axes.
  - Arc coverage comes from atan2 of each point around the centre. Circle error compares the result against equal axes.
- **Line**: a least-squares line through the points, with error as squared perpendicular distance.
- **Polyline, triangle, quadrilateral**:
  - Corner candidates are found by maximum deviation, then sorted and refined in a loop: points are reassigned to their nearest segment and the segments refitted.
  - Error is point-to-segment distance with t clamped to [0, 1].
  - Polyline vertices closer than 3× the tolerance are merged.
  - The rectangle and square variants average opposite edges to get the half-extents, take the rotation from atan2, and blend between the fitted and regularised corners.

### Touch input → strokes, per frame
- **Input** (`SiGestureRecognizer`, an in-house UIGestureRecognizer subclass):
  - For each event it walks **`coalescedTouchesForTouch:`**, so it gets every 240 Hz Pencil sample rather than one per frame. Each sample is stamped with `CACurrentMediaTime`.
  - It also stores **`predictedTouchesForTouch:`** in a dictionary per touch, and handles **`touchesEstimatedPropertiesUpdated:`**. That last one matches late force and altitude updates back to the stored sample by `estimationUpdateIndex` and replaces it; this is the `unhideEstimatedPressure` path in `ValkyrieStrokeSegment`.
- **Threading**:
  - `ValkyrieStrokeMode append:` hands each batch of points to the mode with `dispatch_async` on a private serial queue, so the main thread never builds geometry.
  - Every mode runs through `ValkyrieModeQueue`: `enqueue:`, then `tickWithFlags:`, which leads to the mode's `tick:`. The ticks happen under the queue's lock (`dispatchSync:`, `tickReentrantSafeWithFlags:`).
  - Ticks are driven by the canvas display link (`ValkyrieEngine canvasDisplayLinkCallback:`, `ValkyrieView _drawRect:` with `preferredFramesPerSecond`, which is ProMotion-aware).
  - Other work has its own named queues: "Valkyrie Asynchronous Queue", "ValkyrieDevice Asynchronous Queue", "ValkyrieCompositeMode API Queue" and "ValkyrieDocument Duplicate Queue".
- **Per tick**:
  - The point array (`ValkyrieStrokePointArray`) is a committed prefix plus a predicted suffix (`initCombiningArray:overlayedWithArray:`).
  - Smoothing runs on both parts (`movingAverage…isPredicted:`, `streamlineControlPoints:predicted:`, `executeFFTSmoothing:predicted:`). `separate:committedResult:predictedResult:` then splits the resulting plots at the first predicted one.
  - Committed plots are added to the stroke buffer for good. Predicted plots go into the throwaway "predicted" attachments and are redrawn on each tick.
- **StreamLine maths** (`ValkyrieStroke`):
  - The moving-average window is at most `120·amount^2.2` points.
  - For point i of n in a window w: `wf = (max(0, w − (n − i)) / (w − 1))^(1 − s)` and `f = s + wf·(wf − s)`, where s is the strength. Then position += f·(target − position) on each axis.
  - Pressure gets the same treatment with its own strength (`pressureStrength`).
  - So the newest points (near the pen) are smoothed least and older points within the window are pulled harder. The line catches up behind the pen instead of lagging at the tip.

## Status
All three requested passes and the follow-up items are done.

Remaining unknowns are small:
- What shader id 34 does as a special case.
- The exact eigen-solver for the ellipse (`FUN_10095e9ec`).
- The UI names for document blend modes 29–33.

1. Run full Ghidra analysis (it takes a while) so ObjC calls resolve to names, then decompile `ValkyrieBrush` (539 methods; how each archive key is read and clamped) and `ValkyrieStrokeModeRenderer`.
2. Get the shader IR: the metallib holds AIR bitcode. `llvm-dis` isn't installed (`dnf install llvm`). It would give the exact blend-mode, grain and wet-mix maths.
3. Decompile the `ValkyrieLayerChunk` and `LayerLoadBatcher` paths for the tile cache and eviction policy.
4. Decompile QuickShape, ColorDrop and Liquify.
